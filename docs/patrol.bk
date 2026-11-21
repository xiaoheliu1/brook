// patrol.ms —— 电量巡检：低电量自动进省电模式
// 宿主函数：log(msg)、battery_level()、find_text(t) -> (found, x, y)、tap(x, y)

func level_tag(lv) {
    if lv >= 80 {
        "良好"
    } else if lv >= 30 {
        "可用"
    } else {
        "告急"
    }
}

// 连续按"减"把亮度降到目标以下，返回按键次数
func step_down_to(lv, target) {
    let pressed = 0
    while lv > target && pressed < 20 {      // pressed 上限防死循环
        lv = lv - 5
        pressed = pressed + 1
    }
    pressed
}

func main() {
    let lv = battery_level()
    log("当前电量 " + to_str(lv) + "%（" + level_tag(lv) + "）")

    if lv < 30 {
        log("进入省电流程")
        let n = step_down_to(lv, 10)

        let btn = find_text("省电模式")
        if btn.0 {
            tap(btn.1, btn.2)
            log("已点击省电模式")
        } else {
            log("找不到省电按钮，需人工处理")
        }

        log("共按下 " + to_str(n) + " 次减键")
    } else {
        log("电量充足，无需处理")
    }
}
