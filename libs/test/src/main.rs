use clap::Clap;

/// Tests garbage collectors
#[derive(Clap, Debug)]
#[clap(author, version)]
pub struct GcTestOpts {
    #[clap(arg_enum, default_value = "GcType::all()")]
    #[clap(subcommand)]
    subcommand: Subcommand
}

/// The type of garbage collector
#[derive(Clap, Debug)]
enum GcType {
    Simple
}
impl GcType {
    fn all() -> Vec<GcType> {
        vec![GcType::Simple]
    }
    fn is_supported(&self) -> bool {
        match *self {
            GcType::Simple => cfg!(feature = "simple")
        }
    }
    fn is_sync(&self) -> bool {
        match *self {
            GcType::Simple =>
        }
    }
}

#[derive(Clap)]
pub enum Subcommand {
    Example(ExampleOpts)
}

/// Run the examples
#[derive(Clap)]
pub struct ExampleOpts {
    /// The size of the binary trees
    #[clap(long)]
    binary_trees_size: Option<u32>,
    /// The list of examples to name,
    /// defaults to all of them
    #[clap(default_value = "ExampleKind::all()")]
    #[clap(arg_enum)]
    examples: Vec<ExampleKind>
}
#[derive(Clap, Debug, PartialEq)]
pub enum ExampleKind {
    BinaryTrees,
    BinaryTreesParallel
}
impl ExampleKind {
    fn all() -> Vec<ExampleKind>
}

fn main() -> anyhow::Result<()> {
    let opts = GcTestOpts::parse();
    match opts {

    }
}