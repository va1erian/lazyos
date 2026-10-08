// Brick Breaker: an Arkanoid-style game drawn on a Canvas.
//
// Controls: Left/Right (or A/D) or the mouse move the paddle, Space or a
// click launches the ball, P pauses.
//
// Functions cannot see top-level variables, so the whole game lives in
// form.state. Each event copies it into a local map `s`, works on it through
// functions called in method style (`s.update(dt)` runs `fn update(dt)` with
// `this` bound to `s`) and stores it back once at the end.
//
// The game's modes: "title" (before the first serve), "ready" (the ball sits
// on the paddle), "playing", "paused", "over" and "won".

// ---- Events ----------------------------------------------------------------

fn form_load() {
    form.state = new_game();
    game_canvas.focus();          // so the arrow keys reach the canvas
}

fn game_canvas_frame(dt) {
    let s = form.state;
    s.update(dt);
    s.draw();
    form.state = s;
}

fn game_canvas_key_down(key) {
    let s = form.state;
    if key == "space" {
        s = s.start_or_launch();
    } else if key == "p" {
        s.toggle_pause();
    }
    form.state = s;
}

fn game_canvas_mouse_move(x, y) {
    let s = form.state;
    if s.mode != "paused" {
        s.paddle_x = clamp(x - s.paddle_w / 2.0, 0.0, s.width - s.paddle_w);
    }
    form.state = s;
}

fn game_canvas_mouse_down(x, y, button) {
    if button == "left" {
        form.state = form.state.start_or_launch();
    }
}

// ---- Setting up ------------------------------------------------------------

// A fresh game: three lives, a full wall of bricks, the ball on the paddle.
fn new_game() {
    let s = #{
        width: game_canvas.width.to_float(),
        height: game_canvas.height.to_float(),
        mode: "title",
        score: 0,
        lives: 3,
        paddle_w: 96.0, paddle_h: 14.0, paddle_x: 0.0, paddle_y: 0.0,
        ball_x: 0.0, ball_y: 0.0, ball_vx: 0.0, ball_vy: 0.0, ball_r: 7.0,
        speed: 300.0,             // pixels per second; it creeps up during play
        bricks: [],
        bricks_left: 0,
    };
    s.paddle_x = (s.width - s.paddle_w) / 2.0;
    s.paddle_y = s.height - 40.0;
    s.bricks = make_bricks(s.width);
    s.bricks_left = s.bricks.len();
    s.stick_ball();
    s
}

// Six rainbow rows of ten bricks; the top two rows take two hits.
fn make_bricks(width) {
    let colors = [[231, 76, 60], [230, 126, 34], [241, 196, 15],
                  [46, 204, 113], [52, 152, 219], [155, 89, 182]];
    let columns = 10;
    let margin = 16.0;
    let gap = 4.0;
    let w = (width - 2.0 * margin - gap * (columns - 1)) / columns;
    let bricks = [];
    for row in 0..colors.len() {
        for column in 0..columns {
            bricks.push(#{
                x: margin + column * (w + gap),
                y: 64.0 + row * 24.0,
                w: w,
                h: 20.0,
                hits: if row < 2 { 2 } else { 1 },
                points: if row < 2 { 20 } else { 10 },
                rgb: colors[row],
            });
        }
    }
    bricks
}

// ---- Rules -----------------------------------------------------------------

// Space or a click: start a new game after the last one ended, then serve.
fn start_or_launch() {
    let s = this;
    if s.mode == "over" || s.mode == "won" {
        s = new_game();
    }
    if s.mode == "title" || s.mode == "ready" {
        // Serve straight up, give or take a few degrees.
        let angle = (random() - 0.5) * 0.6;
        s.ball_vx = s.speed * sin(angle);
        s.ball_vy = -s.speed * cos(angle);
        s.mode = "playing";
    }
    s
}

fn toggle_pause() {
    if this.mode == "playing" {
        this.mode = "paused";
    } else if this.mode == "paused" {
        this.mode = "playing";
    }
}

// Puts the ball on top of the paddle's middle.
fn stick_ball() {
    this.ball_x = this.paddle_x + this.paddle_w / 2.0;
    this.ball_y = this.paddle_y - this.ball_r - 1.0;
}

// Advances the game by `dt` seconds.
fn update(dt) {
    if this.mode != "title" && this.mode != "ready" && this.mode != "playing" {
        return;
    }
    this.move_paddle(dt);
    if this.mode != "playing" {
        this.stick_ball();
        return;
    }
    this.speed = min(this.speed + 4.0 * dt, 650.0);
    // Small steps, so a fast ball cannot skip through a 20-pixel brick.
    let steps = max(1, (this.speed * dt / 4.0).ceiling().to_int());
    for step in 0..steps {
        this.move_ball(dt / steps);
        if this.mode != "playing" {
            break;
        }
    }
}

fn move_paddle(dt) {
    let direction = 0.0;
    if game_canvas.is_key_down("left") || game_canvas.is_key_down("a") { direction -= 1.0; }
    if game_canvas.is_key_down("right") || game_canvas.is_key_down("d") { direction += 1.0; }
    let x = this.paddle_x + direction * 480.0 * dt;
    this.paddle_x = clamp(x, 0.0, this.width - this.paddle_w);
}

fn move_ball(dt) {
    let r = this.ball_r;
    this.ball_x += this.ball_vx * dt;
    this.ball_y += this.ball_vy * dt;

    // The side walls and the bottom of the score bar.
    if this.ball_x < r { this.ball_x = r; this.ball_vx = abs(this.ball_vx); }
    if this.ball_x > this.width - r { this.ball_x = this.width - r; this.ball_vx = -abs(this.ball_vx); }
    if this.ball_y < 32.0 + r { this.ball_y = 32.0 + r; this.ball_vy = abs(this.ball_vy); }

    // The paddle: the further from its middle the ball lands, the wider the
    // angle it leaves at (up to about 60 degrees).
    if this.ball_vy > 0.0 && rects_overlap(this.ball_x - r, this.ball_y - r, 2.0 * r, 2.0 * r,
                                           this.paddle_x, this.paddle_y, this.paddle_w, this.paddle_h) {
        let middle = this.paddle_x + this.paddle_w / 2.0;
        let offset = clamp((this.ball_x - middle) / (this.paddle_w / 2.0), -1.0, 1.0);
        this.ball_vx = this.speed * sin(offset * 1.05);
        this.ball_vy = -this.speed * cos(offset * 1.05);
        this.ball_y = this.paddle_y - r;
    }

    this.hit_brick();

    // Missed: lose a life, or the game.
    if this.ball_y - r > this.height {
        this.lives -= 1;
        if this.lives > 0 {
            this.mode = "ready";
            this.stick_ball();
        } else {
            this.mode = "over";
        }
    }
}

// Bounces the ball off the first brick it touches and damages that brick.
fn hit_brick() {
    let r = this.ball_r;
    let left = this.ball_x - r;
    let top = this.ball_y - r;
    let size = 2.0 * r;
    for i in 0..this.bricks.len() {
        let b = this.bricks[i];
        if b.hits > 0 && rects_overlap(left, top, size, size, b.x, b.y, b.w, b.h) {
            // Bounce along the axis the ball went in least.
            let push_x = min(left + size - b.x, b.x + b.w - left);
            let push_y = min(top + size - b.y, b.y + b.h - top);
            // Bounce, and move the ball just outside the brick on that side,
            // so the next step cannot hit the same brick again.
            if push_x < push_y {
                let from_left = this.ball_x < b.x + b.w / 2.0;
                this.ball_vx = if from_left { -abs(this.ball_vx) } else { abs(this.ball_vx) };
                this.ball_x = if from_left { b.x - r } else { b.x + b.w + r };
            } else {
                let from_above = this.ball_y < b.y + b.h / 2.0;
                this.ball_vy = if from_above { -abs(this.ball_vy) } else { abs(this.ball_vy) };
                this.ball_y = if from_above { b.y - r } else { b.y + b.h + r };
            }
            this.bricks[i].hits = b.hits - 1;
            if b.hits == 1 {
                this.score += b.points;
                this.bricks_left -= 1;
            } else {
                this.score += 5;
            }
            this.speed = min(this.speed + 6.0, 650.0);
            this.keep_speed();
            if this.bricks_left == 0 {
                this.mode = "won";
            }
            return;
        }
    }
}

// Rescales the ball's velocity to the current speed, keeping its direction.
fn keep_speed() {
    let length = sqrt(this.ball_vx * this.ball_vx + this.ball_vy * this.ball_vy);
    if length > 0.0 {
        this.ball_vx = this.ball_vx / length * this.speed;
        this.ball_vy = this.ball_vy / length * this.speed;
    }
}

// ---- Drawing ---------------------------------------------------------------

fn draw() {
    game_canvas.clear("#101018");
    this.draw_bricks();
    game_canvas.fill_round_rect(this.paddle_x, this.paddle_y, this.paddle_w, this.paddle_h,
                                7, "#ecf0f1");
    game_canvas.fill_circle(this.ball_x, this.ball_y, this.ball_r, "#ffe066");
    this.draw_score_bar();
    this.draw_message();
}

fn draw_bricks() {
    for b in this.bricks {
        if b.hits > 0 {
            // A brick that still takes two hits is drawn darker.
            let k = if b.hits > 1 { 0.55 } else { 1.0 };
            let red = b.rgb[0] * k;
            let green = b.rgb[1] * k;
            let blue = b.rgb[2] * k;
            game_canvas.fill_round_rect(b.x, b.y, b.w, b.h, 3, rgb(red, green, blue));
            game_canvas.fill_rect(b.x + 3, b.y + 2, b.w - 6, 3,
                                  rgb(red + 40, green + 40, blue + 40));
        }
    }
}

fn draw_score_bar() {
    game_canvas.fill_rect(0, 0, this.width, 32, "#1c1c2c");
    game_canvas.text(12, 7, `Score: ${this.score}`, "#ffffff", 16);
    for i in 0..this.lives {
        game_canvas.fill_circle(this.width - 20 - i * 18, 16, 6, "#e74c3c");
    }
}

// The banner for the modes that wait for the player.
fn draw_message() {
    let y = this.height / 2.0 + 20.0;
    if this.mode == "title" {
        centered("BRICK BREAKER", y - 30.0, 32, "#f5b041");
        centered("Press Space or click to start", y + 16.0, 16, "#ffffff");
        centered("Left/Right or the mouse to move · P to pause", y + 40.0, 13, "#9aa0b4");
    } else if this.mode == "ready" {
        centered("Press Space to launch", y, 16, "#ffffff");
    } else if this.mode == "paused" {
        centered("Paused — press P", y, 20, "#ffffff");
    } else if this.mode == "over" {
        centered("Game over — press Space", y, 24, "#e74c3c");
        centered(`Final score: ${this.score}`, y + 34.0, 16, "#ffffff");
    } else if this.mode == "won" {
        centered("You win!", y, 32, "#2ecc71");
        centered(`Score: ${this.score} — press Space to play again`, y + 44.0, 16, "#ffffff");
    }
}

// Draws `message` centred across the canvas, `y` pixels from the top.
fn centered(message, y, size, color) {
    let w = game_canvas.text_width(message, size);
    game_canvas.text((game_canvas.width - w) / 2.0, y, message, color, size);
}
