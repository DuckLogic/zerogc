
/// Abstracts over thread safe/unsafe versions of [once_cell::sync::OnceCell]
pub trait OnceCell<T>: Sized {
    fn new() -> Self;
    fn get_or_init(&self, func: impl FnOnce() -> T) -> &T;
    fn get(&self) -> Option<&T>;
}
#[cfg(feature = "sync")]
impl<T> OnceCell<T> for once_cell::sync::OnceCell<T> {
    #[inline]
    fn new() -> Self {
        Default::default()
    }

    #[inline]
    fn get_or_init(&self, func: impl FnOnce() -> T) -> &T {
        once_cell::sync::OnceCell::get_or_init(self, func)
    }

    #[inline]
    fn get(&self) -> Option<&T> {
        once_cell::sync::OnceCell::get(self)
    }
}
impl<T> OnceCell<T> for once_cell::unsync::OnceCell<T> {
    #[inline]
    fn new() -> Self {
        Default::default()
    }

    #[inline]
    fn get_or_init(&self, func: impl FnOnce() -> T) -> &T {
        once_cell::unsync::OnceCell::get_or_init(self, func)
    }

    #[inline]
    fn get(&self) -> Option<&T> {
        once_cell::unsync::OnceCell::get(self)
    }
}