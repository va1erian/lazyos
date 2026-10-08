fn hello_button_click() {
    // The greeting the combo shows, or "Hello" before one is chosen.
    let words = greeting_combo.items;
    let index = greeting_combo.selected;
    let word = if index >= 0 && index < words.len() { words[index] } else { "Hello" };
    result_label.text = greeting(word, name_edit.text);
}
