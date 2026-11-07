//! FreeRTOS queues use INTERNAL | 8BIT memory in ESP-IDF heap_idf.c.
//! Callback sends never wait for space. Queue ownership transfers Rust values.
use crate::error::{FirmwareError, Result};
use core::{
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use esp_idf_svc::sys;

/// Aligned internal scratch for receiving large Copy values without stack moves.
pub struct InternalBuffer<T: Copy>(core::ptr::NonNull<MaybeUninit<T>>);
unsafe impl<T: Copy + Send> Send for InternalBuffer<T> {}
impl<T: Copy> InternalBuffer<T> {
    pub fn new() -> Result<Self> {
        let pointer = unsafe {
            sys::heap_caps_aligned_alloc(
                core::mem::align_of::<T>(),
                core::mem::size_of::<T>(),
                sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT,
            )
        };
        core::ptr::NonNull::new(pointer.cast())
            .map(Self)
            .ok_or(FirmwareError::QueueClosed)
    }
}
impl<T: Copy> Drop for InternalBuffer<T> {
    fn drop(&mut self) {
        unsafe {
            sys::heap_caps_free(self.0.as_ptr().cast());
        }
    }
}

pub struct InternalQueue<T> {
    handles: Vec<sys::QueueHandle_t>,
    capacity: usize,
    written: AtomicUsize,
    read: AtomicUsize,
    sending: AtomicBool,
    receiving: AtomicBool,
    marker: PhantomData<T>,
}
unsafe impl<T: Send> Send for InternalQueue<T> {}
unsafe impl<T: Send> Sync for InternalQueue<T> {}
impl<T> InternalQueue<T> {
    pub fn new(capacity: usize) -> Result<Self> {
        assert!(capacity > 0 && core::mem::size_of::<T>() > 0);
        let size = core::mem::size_of::<T>();
        // Internal RAM is fragmented into several heaps. Power-of-two stripes
        // preserve FIFO indexing even when the monotonically increasing cursors wrap.
        let per = (16384 / size).max(1);
        let shards = capacity.div_ceil(per).next_power_of_two();
        let slots = capacity.div_ceil(shards);
        let mut handles = Vec::with_capacity(shards);
        for _ in 0..shards {
            let handle = unsafe { sys::xQueueGenericCreate(slots as _, size as _, 0) };
            if handle.is_null() {
                log::error!(
                    "internal queue allocation failed: {} bytes; free={}, largest={}",
                    slots * size,
                    unsafe {
                        sys::heap_caps_get_free_size(
                            sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT,
                        )
                    },
                    unsafe {
                        sys::heap_caps_get_largest_free_block(
                            sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT,
                        )
                    }
                );
                for handle in handles {
                    unsafe { sys::vQueueDelete(handle) };
                }
                return Err(FirmwareError::QueueClosed);
            }
            handles.push(handle);
        }
        Ok(Self {
            handles,
            capacity,
            written: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            sending: AtomicBool::new(false),
            receiving: AtomicBool::new(false),
            marker: PhantomData,
        })
    }
    pub fn try_send(&self, value: T) -> core::result::Result<(), T> {
        let value = ManuallyDrop::new(value);
        if self.send_pointer(&*value as *const T) {
            Ok(())
        } else {
            Err(ManuallyDrop::into_inner(value))
        }
    }
    pub fn try_send_copy(&self, value: &T) -> bool
    where
        T: Copy,
    {
        self.send_pointer(value as *const T)
    }
    fn send_pointer(&self, value: *const T) -> bool {
        if self
            .sending
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        let written = self.written.load(Ordering::Relaxed);
        let available = written.wrapping_sub(self.read.load(Ordering::Acquire)) < self.capacity;
        let sent = available
            && unsafe {
                sys::xQueueGenericSend(
                    self.handles[written % self.handles.len()],
                    value.cast(),
                    0,
                    0,
                ) != 0
            };
        if sent {
            self.written
                .store(written.wrapping_add(1), Ordering::Release);
        }
        self.sending.store(false, Ordering::Release);
        sent
    }
    pub fn try_recv(&self) -> core::result::Result<T, ()> {
        let mut value = MaybeUninit::<T>::uninit();
        if self.receive_pointer(value.as_mut_ptr()) {
            Ok(unsafe { value.assume_init() })
        } else {
            Err(())
        }
    }
    pub fn try_recv_copy<'a>(&self, output: &'a mut InternalBuffer<T>) -> Option<&'a T>
    where
        T: Copy,
    {
        let pointer = output.0.as_ptr().cast::<T>();
        if self.receive_pointer(pointer) {
            Some(unsafe { &*pointer })
        } else {
            None
        }
    }
    fn receive_pointer(&self, output: *mut T) -> bool {
        if self
            .receiving
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        let read = self.read.load(Ordering::Relaxed);
        let received = read != self.written.load(Ordering::Acquire)
            && unsafe {
                sys::xQueueReceive(self.handles[read % self.handles.len()], output.cast(), 0) != 0
            };
        if received {
            self.read.store(read.wrapping_add(1), Ordering::Release);
        }
        self.receiving.store(false, Ordering::Release);
        received
    }
    pub fn len(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        self.written
            .load(Ordering::Acquire)
            .wrapping_sub(read)
            .min(self.capacity)
    }
}
impl<T> Drop for InternalQueue<T> {
    fn drop(&mut self) {
        if core::mem::needs_drop::<T>() {
            while let Ok(v) = self.try_recv() {
                drop(v);
            }
        }
        for handle in self.handles.drain(..) {
            unsafe { sys::vQueueDelete(handle) };
        }
    }
}
