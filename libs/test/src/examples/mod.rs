use zerogc::{GcContext, Gc};
use slog::Logger;
use crate::{GcTest, GcSyncTest};
use slog::Drain;

mod binary_trees;
pub(crate) mod binary_trees_parallel;


macro_rules! define_example {
    (def @kind sync fn $name:ident($args:ident) $body:expr) => {
        fn $name<GC: GcSyncTest>($args: &[&str]) -> anyhow::Result<()> {
            $body
        }
    };
    (def @kind nosync fn $name:ident($args:ident) $body:expr) => {
        fn $name<GC: GcTest>($args: &[&str]) -> anyhow::Result<()> {
            $body
        }
    };
    ($name:ident, $target_module:ident, $kind:ident) => {
        define_example!(def @kind $kind fn $name(args) {
            let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
            let logger = Logger::root(
                slog_term::FullFormat::new(plain).build().fuse(),
                ::slog::o!("bench" => file!())
            );
            let collector = GC::create_collector(logger);
            self::$target_module::main(&logger, collector, args)
        });
    };
}
define_example!(binary_trees, binary_trees, nosync);
define_example!(binary_trees_parallel, binary_trees_parallel, sync);

#[derive(Copy, Clone)]
pub enum ExampleKind {
    BinaryTrees,
    BinaryTreesParallel
}
#[derive(Default)]
pub struct ExampleArgs {
    binary_trees_size: Option<u32>,
}

macro_rules! run_example {
    ($name:ident, $gc:ident, $($arg:expr),*) => {{
        eprintln!(concat!("Running ", stringify!($name), ":"));
        let owned_args = vec![$(Option::map($arg, |item| item.to_string())),*];
        while let Some(None) = owned_args.last() {
            assert_eq!(owned_args.pop(), None);
        }
        let args = owned_args.iter().enumerate().map(|(index, opt)| opt.as_ref().unwrap_or_else(|| {
            panic!("Arg #{} is None: {:?}", index, owned_args)
        }).as_str()).collect::<Vec<_>>();
        use anyhow::Context;
        let () = ($name::<$gc>)(&*args).with_context(|| format!("Running example {:?}", stringify!($name)))?;
    }};
}
/// Run all the example for the specified (non-Sync) collector
pub fn run_examples_non_sync<GC: GcTest>(args: ExampleArgs) -> anyhow::Result<()> {
    run_example!(binary_trees, GC, args.binary_trees_size);
    Ok(())
}
/// Run the examples for the specified Sync collector,
/// including all the non-sync examples;
pub fn run_examples_sync<GC: GcSyncTest>(args: ExampleArgs) -> anyhow::Result<()> {
    run_examples_non_sync::<GC>(args);
    run_example!(binary_trees_parallel, GC, args.binary_trees_size);
    Ok(())
}
