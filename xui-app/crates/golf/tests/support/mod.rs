//! A game without a window: generation messages arrive on a channel the
//! test hands to the game itself, the way the window does with `Msg::Gen`.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use xui_golf::game::Game;
use xui_golf::{Gen, Params};
use xui_work::Sink;

/// A quick-course game and the receiving end of its generator's messages.
pub fn quick_game() -> (Game, Receiver<Gen>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let sink = Sink::from_fn(move |message| tx.lock().unwrap().send(message).is_ok());
    (Game::with_params(Params::quick(4), 96, sink), rx)
}

/// Delivers what the generator has said so far, then ticks, until the course
/// is up; fails rather than hangs.
pub fn run_until_generated(
    game: &mut Game,
    messages: &Receiver<Gen>,
    size: (usize, usize),
    now: Instant,
) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while game.run.is_none() {
        assert!(Instant::now() < deadline, "generation did not finish");
        while let Ok(message) = messages.try_recv() {
            game.accept(message);
        }
        game.tick(size.0, size.1, now);
        std::thread::sleep(Duration::from_millis(5));
    }
}
