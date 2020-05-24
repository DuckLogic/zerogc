import sys


def bottom_up_tree(depth):
    if depth > 0:
        depth -= 1
        return bottom_up_tree(depth), bottom_up_tree(depth)
    else:
        return None


def item_check(tree):
    if tree is not None:
        first, second = tree
        return 1 + item_check(first) + item_check(second)
    else:
        return 1


def run(n, min_depth, max_depth):
    stretch_depth = max_depth + 1
    stretch_tree = bottom_up_tree(stretch_depth)
    print("stretch tree of depth {}\t check: {}".format(
        stretch_depth, item_check(stretch_tree)
    ))
    del stretch_tree

    long_lived_tree = bottom_up_tree(max_depth)

    for depth in range(min_depth, max_depth, 2):
        iterations = 1 << (max_depth - depth + min_depth)
        check = 0
        for i in range(iterations):
            check += item_check(bottom_up_tree(depth))
        print("{}\t trees of depth {}\t check: {}".format(iterations, depth, check))

    print("long lived tree of depth {}\t check: {}\n".format(
        max_depth, item_check(long_lived_tree)
    ))


if __name__ == "__main__":
    try:
        n = int(sys.argv[1])
    except IndexError:
        n = 10
    min_depth = 4
    max_depth = min_depth + 2
    if max_depth < n:
        max_depth = n
    run(n, min_depth, max_depth)

