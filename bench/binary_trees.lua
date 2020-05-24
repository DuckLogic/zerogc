-- The Computer Language Benchmarks Game
-- https://salsa.debian.org/benchmarksgame-team/benchmarksgame/
-- contributed by Mike Pall
-- modified by Sebastian Engel to be parallel, derived from mandelbrot-lua-6

-- called with the following arguments on the command line;
-- 1: Initial depth of the tree
-- 2: number of children to spawn (defaults to 6, which works well on 4-way)
-- If this is a child, then there will be additional parameters;
-- 3: current tree depth
-- 4: chunk start
-- 5: chunk end


local write = io.write

local function BottomUpTree(depth)
  if depth > 0 then
    depth = depth - 1
    local left, right = BottomUpTree(depth), BottomUpTree(depth)
    return { left, right }
  else
    return { }
  end
end

local function ItemCheck(tree)
  if tree[1]  then
    return 1 + ItemCheck(tree[1]) + ItemCheck(tree[2])
  else
    return 1
  end
end

local N = tonumber(arg and arg[1]) or 10
local mindepth = 4
local maxdepth = mindepth + 2
if maxdepth < N then maxdepth = N end

do
  local stretchdepth = maxdepth + 1
  local stretchtree = BottomUpTree(stretchdepth)
  write(string.format("stretch tree of depth %d\t check: %d\n",
    stretchdepth, ItemCheck(stretchtree)))
end

local longlivedtree = BottomUpTree(maxdepth)

for depth=mindepth,maxdepth,2 do
  local iterations = 2 ^ (maxdepth - depth + mindepth)
  local check = 0
  for i=1,iterations do
    check = check + ItemCheck(BottomUpTree(depth))
  end
  write(string.format("%d\t trees of depth %d\t check: %d\n", iterations, depth, check))
end

write(string.format("long lived tree of depth %d\t check: %d\n",
  maxdepth, ItemCheck(longlivedtree)))

