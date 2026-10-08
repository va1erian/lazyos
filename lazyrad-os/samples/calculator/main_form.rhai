fn form_load() {
    form.state.accumulator = 0.0;
    form.state.pending = "";
    form.state.entering = "";
    display_label.text = "0";
}

fn digit_0_click() { type_digit("0"); }
fn digit_1_click() { type_digit("1"); }
fn digit_2_click() { type_digit("2"); }
fn digit_3_click() { type_digit("3"); }
fn digit_4_click() { type_digit("4"); }
fn digit_5_click() { type_digit("5"); }
fn digit_6_click() { type_digit("6"); }
fn digit_7_click() { type_digit("7"); }
fn digit_8_click() { type_digit("8"); }
fn digit_9_click() { type_digit("9"); }

fn add_button_click() { choose_operator("+"); }
fn subtract_button_click() { choose_operator("-"); }
fn multiply_button_click() { choose_operator("*"); }
fn divide_button_click() { choose_operator("/"); }

fn equals_button_click() {
    commit();
    form.state.pending = "";
    display_label.text = format_number(form.state.accumulator);
}

fn clear_button_click() {
    form.state.accumulator = 0.0;
    form.state.pending = "";
    form.state.entering = "";
    display_label.text = "0";
}

fn type_digit(digit) {
    form.state.entering = `${form.state.entering}${digit}`;
    display_label.text = form.state.entering;
}

fn choose_operator(operator) {
    commit();
    form.state.pending = operator;
}

fn commit() {
    let typed = form.state.entering;
    if typed != "" {
        form.state.accumulator = calc::apply(
            form.state.accumulator,
            form.state.pending,
            parse_float(typed),
        );
        form.state.entering = "";
    }
}

fn format_number(value) {
    if value == value.floor() {
        `${value.to_int()}`
    } else {
        `${value}`
    }
}
