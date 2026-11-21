// 示例：斐波那契（递归 + if 语句 + 末位表达式返回）
// 文件末尾约定：脚本定义函数，宿主通过 engine.call 调用。

func fib(n) {
    if n < 2 {
        return n
    }
    fib(n - 1) + fib(n - 2)
}
