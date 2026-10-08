fn apply(left, operator, right) {
    if operator == "+" {
        left + right
    } else if operator == "-" {
        left - right
    } else if operator == "*" {
        left * right
    } else if operator == "/" {
        // Dividing by zero keeps the previous value rather than showing a
        // bogus result.
        if right == 0.0 {
            left
        } else {
            left / right
        }
    } else {
        right
    }
}
