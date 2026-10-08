fn add_button_click() {
    let text = todo_edit.text;
    if text != "" {
        let items = todo_list.items;
        items.push(text);
        todo_list.items = items;
        todo_edit.text = "";
    }
}

fn remove_button_click() {
    let index = todo_list.selected;
    let items = todo_list.items;
    if index >= 0 && index < items.len() {
        items.remove(index);
        todo_list.items = items;
    }
}
