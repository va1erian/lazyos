// Picture Viewer: a picture viewer in the manner of Windows XP's Picture and
// Fax Viewer, written in LazyRAD (docs/lazyrad-pictures.md).
//
// The PictureBox does the drawing (fit, zoom, rotation, panning by drag);
// this script keeps the list of pictures in the current folder and maps the
// toolbar, keys and wheel to it:
//
//   Left/Right, Page Up/Down, Space, Backspace, the wheel: previous / next
//   Home / End: first / last          Ctrl+wheel: zoom
//   0: best fit   1: actual size      double-click: switch between the two
//   K / L: rotate right / left        F11: slide show, Escape: stop it
//   O: open       E: edit (hands the picture to Paint through mimed)
//
// The viewer starts on the picture it was opened with (`app.documents`: a
// picture double-clicked in Files, whose folder the player may read), or on
// its sample pictures.

fn form_load() {
    form.state.folder = "";
    form.state.files = [];
    form.state.index = -1;
    form.state.error = "";
    let documents = app.documents;
    if documents.len() > 0 {
        open_path(documents[0]);
    } else {
        open_folder(paths::join(app.path, "samples"), "");
    }
    picture1.focus();
}

// ------------------------------------------------------------ the folder ---

// Show `path` and make its folder the one Previous and Next page through.
fn open_path(path) {
    let parts = paths::split(path);
    open_folder(parts[0], parts[1]);
}

// Page through the pictures in `folder`, starting at `name` (the first when
// empty). A folder the viewer may not list still shows the named picture.
fn open_folder(folder, name) {
    let files = [];
    try {
        files = paths::pictures_in(folder);
    } catch {
        // Not listable (or missing): only the named picture, if any.
    }
    let index = if name == "" { 0 } else { paths::position(files, name) };
    if index < 0 {
        files.push(name);
        index = files.len() - 1;
    }
    form.state.folder = folder;
    form.state.files = files;
    show(index);
}

// Show picture `index` of the folder, wrapping around at either end.
fn show(index) {
    let files = form.state.files;
    let count = files.len();
    if count == 0 {
        form.state.index = -1;
        picture1.clear();
        form.title = "Picture Viewer";
        update_status();
        return;
    }
    index = ((index % count) + count) % count;
    form.state.index = index;
    let name = files[index];
    try {
        picture1.load(file_read_bytes(paths::join(form.state.folder, name)));
        picture1.best_fit();
        form.state.error = "";
    } catch (error) {
        picture1.clear();
        form.state.error = `${error}`;
    }
    form.title = `${name} - Picture Viewer`;
    update_status();
}

fn current_path() {
    if form.state.index < 0 {
        return "";
    }
    paths::join(form.state.folder, form.state.files[form.state.index])
}

// "sunset.jpg   1024 x 768 JPEG   3 of 12   50%".
fn update_status() {
    let files = form.state.files;
    if files.len() == 0 {
        status_label.text = "No pictures here. Click Open... to choose one.";
        return;
    }
    let name = files[form.state.index];
    let where_ = `${form.state.index + 1} of ${files.len()}`;
    if form.state.error != "" {
        status_label.text = `${name}   ${where_}   Cannot show this picture: ${form.state.error}`;
        return;
    }
    let zoom = picture1.zoom.round().to_int();
    let size = `${picture1.image_width} x ${picture1.image_height} ${picture1.format}`;
    let turned = if picture1.rotation == 0 { "" } else { `   turned ${picture1.rotation} degrees` };
    status_label.text = `${name}   ${size}   ${where_}   ${zoom}%${turned}`;
}

// ------------------------------------------------------------- commands ---
//
// None of these may share a PictureBox method's name and arity: Rhai would
// call the script function for `picture1.best_fit()` too (and recurse).

fn previous() { show(form.state.index - 1); }
fn next() { show(form.state.index + 1); }

fn view_best_fit() { picture1.best_fit(); update_status(); }
fn view_actual_size() { picture1.actual_size(); update_status(); }
fn view_zoom_in() { picture1.zoom_in(); update_status(); }
fn view_zoom_out() { picture1.zoom_out(); update_status(); }
fn turn_right() { picture1.rotate_cw(); update_status(); }
fn turn_left() { picture1.rotate_ccw(); update_status(); }

fn open_picture() {
    stop_show();
    open_file_dialog(
        "Open a picture",
        "Pictures|*.png,*.jpg,*.jpeg,*.bmp,*.gif;All files|*.*",
        |path| {
            if type_of(path) == "string" {
                open_path(path);
            }
            picture1.focus();
        },
        "folder"
    );
}

// Hand the picture to the editor registered for it (Paint, for a PNG).
fn edit_picture() {
    let path = current_path();
    if path == "" {
        return;
    }
    try {
        sys::mimed::open(path, "edit");
        status_label.text = `Opening ${paths::split(path)[1]} for editing...`;
    } catch (error) {
        status_label.text = `No editor could open this picture: ${error}`;
    }
}

fn toggle_show() {
    if slide_timer.enabled { stop_show(); } else { start_show(); }
}

fn start_show() {
    if form.state.files.len() < 2 {
        return;
    }
    slide_timer.enabled = true;
    slideshow_button.text = "Stop Show";
}

fn stop_show() {
    slide_timer.enabled = false;
    slideshow_button.text = "Slide Show";
}

// ------------------------------------------------------------- handlers ---

fn slide_timer_tick() { next(); }

fn prev_button_click() { previous(); picture1.focus(); }
fn next_button_click() { next(); picture1.focus(); }
fn fit_button_click() { view_best_fit(); picture1.focus(); }
fn actual_button_click() { view_actual_size(); picture1.focus(); }
fn slideshow_button_click() { toggle_show(); picture1.focus(); }
fn zoom_in_button_click() { view_zoom_in(); picture1.focus(); }
fn zoom_out_button_click() { view_zoom_out(); picture1.focus(); }
fn rotate_cw_button_click() { turn_right(); picture1.focus(); }
fn rotate_ccw_button_click() { turn_left(); picture1.focus(); }
fn open_button_click() { open_picture(); }
fn edit_button_click() { edit_picture(); }

fn picture1_key_down(key) {
    switch key {
        "left" | "up" | "page_up" | "backspace" => previous(),
        "right" | "down" | "page_down" | "space" | "enter" => next(),
        "home" => show(0),
        "end" => show(form.state.files.len() - 1),
        "0" | "b" => view_best_fit(),
        "1" | "a" => view_actual_size(),
        "k" => turn_right(),
        "l" => turn_left(),
        "f11" => toggle_show(),
        "escape" => stop_show(),
        "o" => open_picture(),
        "e" => edit_picture(),
        _ => (),
    }
}

// The wheel pages through the folder; with Ctrl it zooms.
fn picture1_wheel(delta, ctrl) {
    if ctrl {
        if delta > 0 { view_zoom_in(); } else { view_zoom_out(); }
    } else if delta > 0 {
        previous();
    } else {
        next();
    }
}

fn picture1_double_click(x, y) {
    if picture1.fit { view_actual_size(); } else { view_best_fit(); }
}
