public class BinaryTree {
    private final BinaryTree first, second;
    public BinaryTree(BinaryTree first, BinaryTree second) {
        this.first = first;
        this.second = second;
    }
    public static BinaryTree bottomUpTree(int depth) {
        if (depth > 0) {
            depth -= 1;
            return new BinaryTree(bottomUpTree(depth), bottomUpTree(depth));
        } else {
            return null;
        }
    }
    public static int itemCheck(BinaryTree tree) {
        if (tree != null) {
            return 1 + itemCheck(tree.first) + itemCheck(tree.second);
        } else {
            return 1;
        }
    }
    public static void run(int n, int minDepth, int maxDepth) {
        {
            int stretchDepth = maxDepth + 1;
            BinaryTree stretchTree = bottomUpTree(stretchDepth);
            System.out.println("stretch tree of depth " + stretchDepth +
                    "\t check: " + itemCheck(stretchTree)
            );
        }
        BinaryTree longLivedTree = bottomUpTree(maxDepth);
        for (int depth = minDepth; depth < maxDepth; depth += 2) {
            int iterations = 1 << (maxDepth - depth + minDepth);
            int check = 0;
            for (int i = 0; i < iterations; i += 1) {
                check += itemCheck(bottomUpTree(depth));
            }
            System.out.println(Integer.toString(iterations) + "\t trees of depth " +
                    depth + "\t check: " + check);
        }
        System.out.println("long lived tree of depth " + maxDepth +
                "\t check: " + itemCheck(longLivedTree));
    }
    public static void main(String[] args) {
        int n;
        if (args.length >= 1) {
            n = Integer.parseInt(args[0]);
        } else {
            n = 10;
        }
        int min_depth = 4;
        int max_depth = min_depth + 2;
        if (max_depth < n) max_depth = n;
        BinaryTree.run(n, min_depth, max_depth);
    }
}