#!/usr/bin/env python3
import sys

from subprocess import run, CalledProcessError

try:
    import click
except ImportError:
    print("Unable to import required dependency: click", file=sys.stderr)
    sys.exit(2)
    raise AssertionError

from click import ClickException

if sys.version_info[:2] < (3, 9):
    print("Unsupported python version:", '.'.join(sys.version_info[:3]), file=sys.stderr)
    print("Requires at least Python 3.9")
    sys.exit(2)
    raise AssertionError

from dataclasses import dataclass

@dataclass
class ExampleData:
    package: str
    args: list[str]

EXAMPLES = {
    "binary_trees": ExampleData(package='zerogc-simple', args=['12']),
    "binary_trees_parallel": ExampleData(package='zerogc-simple', args=['12']),
}

def normalize_example(name):
    return name.replace('-', '_')

def print_seperator():
    print()
    print('-' * 8)
    print()

@click.command()
@click.option('--all', is_flag=True, help="Run **all** the examples")
@click.option('--example', '-e', 'examples', multiple=True, help="The name of the example to run")
@click.option('--list', '-l', 'list_examples', is_flag=True, help="List available examples")
@click.option('--release', is_flag=True, help="Compile code in release mode")
def run_examples(list_examples: bool, examples: list[str], all: bool, release: bool):
    if all:
        if examples:
            raise ClickException("Should not specify explicit examples along with `-all`")
        else:
            examples = sorted(EXAMPLES.keys())
    if list_examples and examples:
        raise ClickException("Should not specify any examples along with '--list'")
    if not examples:
        # Imply '--list' if nothing else is specified
        list_examples = True
    if list_examples:
        click.echo("Listing available examples:    [Type --help for more info]")
        for example in EXAMPLES.keys():
            click.echo(f"  {example}")
        sys.exit()
    # Normalize all names
    examples = list(map(normalize_example, examples))
    extra_cargo_args = []
    if release:
        extra_cargo_args += ['--release']
    for example_name in examples:
        if example_name not in EXAMPLES:
            raise ClickException("Invalid example name: {example_name}")
    for example_name in examples:
        example = EXAMPLES[example_name]
        print(f"Compiling example: {example_name}")
        try:
            run(["cargo", "build", "--example", example_name, '-p', example.package, *extra_cargo_args], check=True)
        except CalledProcessError as e:
            raise ClickException(f"Failed to compile {example_name}")
        print_seperator()
    for index, example_name in enumerate(examples):
        example = EXAMPLES[example_name]
        print("Running example: {example_name}")
        try:
            run(["cargo", "run", "--example", example_name, '-p', example.package, *extra_cargo_args, '--', *example.args], check=True)
        except CalledProcessError:
            raise ClickException(f"Failed to run example: {example_name}")
        if index + 1 != len(examples):
            print_seperator()

if __name__ == "__main__":
    run_examples()
