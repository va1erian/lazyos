// title: Volume wave
// tags: do, audio
// needs: sound card
// expect: master was 32768, put back
//
// Ramp the master volume up and down and watch the tray applet follow, then
// put it back. This changes your real volume while it runs.
let m = sys::audio_mixer::get_master();
let full = 65536;                          // gain_q16: 1.0
for step in 0..21 {
    let x = if step <= 10 { step } else { 20 - step };
    sys::audio_mixer::set_master(full * x / 10, false);
    sleep(150);
}
sys::audio_mixer::set_master(m.gain_q16, m.mute);   // put it back
print(`master was ${m.gain_q16}, put back`);
