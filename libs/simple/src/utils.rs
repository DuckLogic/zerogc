
#[derive(Clone)]
pub struct ThreadId {
    id: std::thread::ThreadId,
    name: Option<String>
}
impl ThreadId {
    pub fn current() -> ThreadId {
        let thread = std::thread::current();
        ThreadId {
            id: thread.id(),
            name: thread.name().map(String::from)
        }
    }
}
impl slog::Value for ThreadId {
    fn serialize(
        &self, _record: &slog::Record,
        key: &'static str,
        serializer: &mut dyn slog::Serializer
    ) -> slog::Result<()> {
        let id = self.id;
        match self.name {
            Some(ref name) => {
                serializer.emit_arguments(key, &format_args!(
                    "{}: {:?}", *name, id
                ))
            },
            None => {
                serializer.emit_arguments(key, &format_args!(
                    "{:?}", id
                ))
            },
        }
    }
}
