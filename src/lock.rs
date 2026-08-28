//! Thread-safe interior mutability primitive.
//!
//! `Lock<T>` exposes the same accessors vireo uses on `Cell<T>`/`RefCell<T>`
//! (`get`/`set` for `Copy` `T`, `borrow`/`borrow_mut`, `replace`/`take`), but is
//! backed by a `Mutex` so the containing struct stays `Send + Sync`. This lets
//! `VireoWindow`/`InputState`/`App` be shared across render threads (wrapped in
//! `Arc`) without rewriting the hundreds of `.get()/.borrow()` call sites.

use std::sync::Mutex;
use std::sync::MutexGuard;

pub struct Lock<T>(Mutex<T>);

impl<T> Lock<T> {
    pub fn new(value: T) -> Self {
        Lock(Mutex::new(value))
    }

    // 注意：`Mutex` 不可重入。不要写出这种结构——
    //   let g = self.field.borrow();  // guard 还活着（没离开作用域）
    //   self.some_method();           // 若 some_method 内部又 self.field.borrow()/borrow_mut()
    //                                  // → 同一线程重入同一 Mutex → 自己等自己 → 静默死锁（release 下无 panic）
    // 持 guard 期间：要么只操作 guard 本身，要么只借「不同」字段，要么让它是临时量、语句尾立即 drop。
    pub fn borrow(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }

    pub fn borrow_mut(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }

    pub fn replace(&self, value: T) -> T {
        std::mem::replace(&mut *self.0.lock().unwrap(), value)
    }
}

impl<T: Copy> Lock<T> {
    pub fn get(&self) -> T {
        *self.0.lock().unwrap()
    }

    pub fn set(&self, value: T) {
        *self.0.lock().unwrap() = value;
    }
}

impl<T: Default> Lock<T> {
    pub fn take(&self) -> T {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}
