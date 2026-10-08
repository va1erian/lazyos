// Paths and picture names for the Picture Viewer. Plain Rhai: nothing here
// touches the filesystem except `pictures_in`, which lists one folder.

// The file name endings the viewer opens (PictureBox decodes these).
fn picture_endings() {
    [".png", ".jpg", ".jpeg", ".bmp", ".gif"]
}

// Whether `name` looks like a picture, by its ending in any case.
fn is_picture(name) {
    let lower = name.to_lower();
    for ending in picture_endings() {
        if lower.ends_with(ending) {
            return true;
        }
    }
    false
}

// [folder, name] of `path`: "/home/user/a.png" gives ["/home/user", "a.png"],
// "/a.png" gives ["/", "a.png"] and "a.png" gives ["", "a.png"].
fn split(path) {
    let cut = -1;
    let i = 0;
    for c in path.chars() {
        if c == '/' {
            cut = i;
        }
        i += 1;
    }
    if cut < 0 {
        return ["", path];
    }
    let folder = if cut == 0 { "/" } else { path.sub_string(0, cut) };
    [folder, path.sub_string(cut + 1)]
}

// `name` inside `folder`.
fn join(folder, name) {
    if folder == "" {
        name
    } else if folder.ends_with("/") {
        folder + name
    } else {
        folder + "/" + name
    }
}

// The pictures in `folder`, sorted by name ignoring case, as the classic
// viewer pages through them. An error if the folder cannot be listed.
fn pictures_in(folder) {
    let names = [];
    for name in dir_list(folder) {
        if is_picture(name) {
            names.push(name);
        }
    }
    names.sort(|a, b| {
        let x = a.to_lower();
        let y = b.to_lower();
        if x < y { -1 } else if x > y { 1 } else { 0 }
    });
    names
}

// The position of `name` in `names`, or -1.
fn position(names, name) {
    let i = 0;
    for candidate in names {
        if candidate == name {
            return i;
        }
        i += 1;
    }
    -1
}
