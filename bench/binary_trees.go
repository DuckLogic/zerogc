package main

import (
    "fmt"
    "os"
    "strconv"
)

type Tree struct {
    first, second *Tree
}

func bottomUpTree(depth int) *Tree {
    if depth > 0 {
        depth -= 1
        return &Tree{bottomUpTree(depth), bottomUpTree(depth)}
    } else {
        return nil
    }
}

func (tree *Tree) itemCheck() int {
    if tree == nil {
        return 1
    } else {
        return 1 + tree.first.itemCheck() + tree.second.itemCheck()
    }
}

func run(n, minDepth, maxDepth int) {
    {
        stretchDepth := maxDepth + 1
        stretchTree := bottomUpTree(stretchDepth)
        fmt.Printf(
            "stretch tree of depth %v\t check: %v\n",
            stretchDepth, stretchTree.itemCheck(),
        )
    }
    longLivedTree := bottomUpTree(maxDepth)
    for depth := minDepth; depth < maxDepth; depth += 2 {
        iterations := 1 << (maxDepth - depth + minDepth)
        check := 0
        for i := 0; i < iterations; i++ {
            check += bottomUpTree(depth).itemCheck()
        }
        fmt.Printf(
            "%v\t trees of depth %v\t check: %v\n",
            iterations, depth, check,
        )
    }
    fmt.Printf(
        "long lived tree of depth %v\t check: %v\n",
        maxDepth, longLivedTree.itemCheck(),
    )
}

func main() {
    n := 10
    if len(os.Args) >= 2 {
        i, err := strconv.Atoi(os.Args[1])
        if err != nil {
            panic(
                fmt.Errorf("Invalid int %g", err),
            )
        }
        n = i
    }
    minDepth := 4
    maxDepth := minDepth + 2
    if maxDepth < n {
        maxDepth = n
    }
    run(n, minDepth, maxDepth)
}