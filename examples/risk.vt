// 示例：订单风控规则（宿主句柄 + 类型推断 + 元组 + if 表达式）
// 宿主需注册：order_total(Order) -> f64, is_vip(Order) -> bool

func discount(amount, vip) {
    if vip { amount * 0.8 } else { amount }
}

func check(order, limit) {
    let total = order_total(order)
    let vip = is_vip(order)
    let d = discount(total, vip)
    if d > limit { (0.0, false) } else { (d, true) }
}
