#!/usr/bin/env node
// Run the sample cartridge in jsnes, a second and independent NES emulator,
// and report what the console did as one JSON object on stdout.
//
// `tools/nes/validate_external.py` installs jsnes under `target/nes-jsnes`
// (npm; never committed, never in the image) and runs this with the ROM the
// image ships. The ROM stores what a test wants to know about the console it
// is running on: the controller byte at $10 and a frame counter at $12/$13.
//
// Usage: JSNES_DIR=<dir> node jsnes_check.js <rom> [frames]

const fs = require("node:fs");
const path = require("node:path");
const { pathToFileURL } = require("node:url");

//: The ROM's first note is A4 (`tools/nes/sample_rom.py`).
const NOTE_HZ = 440;
//: Frames the first note is held (30) — collect its samples.
const NOTE_FRAMES = 30;
//: The sample rate the APU is asked for.
const RATE = 48000;

/// The peak absolute sample of `list`.
function peak(list) {
  let top = 0;
  for (const sample of list) top = Math.max(top, Math.abs(sample));
  return top;
}

/// The frequency with the strongest autocorrelation in `list`, between
/// 100 Hz and 2 kHz: enough to say which note a square wave is.
function dominantHz(list, rate) {
  const from = Math.floor(list.length / 4);
  const to = Math.floor((list.length * 3) / 4);
  const window = list.slice(from, to);
  let best = { hz: 0, score: 0 };
  for (let hz = 100; hz <= 2000; hz += 2) {
    const lag = Math.round(rate / hz);
    if (lag <= 0 || lag >= window.length) continue;
    let score = 0;
    for (let i = 0; i + lag < window.length; i++) score += window[i] * window[i + lag];
    if (score > best.score) best = { hz, score };
  }
  return best.hz;
}

async function main() {
  const romPath = process.argv[2];
  const frames = Number(process.argv[3] ?? 200);
  const jsnesDir = process.env.JSNES_DIR;
  if (!romPath || !jsnesDir) {
    console.error("usage: JSNES_DIR=<dir> node jsnes_check.js <rom> [frames]");
    process.exit(2);
  }
  // jsnes is imported from where the wrapper installed it, not from this
  // file's own location (the tool is checked in, the package is not). Only
  // its ES module entry exports the classes.
  const entry = path.join(jsnesDir, "node_modules", "jsnes", "src", "index.js");
  const { NES, Controller } = await import(pathToFileURL(entry).href);

  const samples = [];
  let frameBuffer = null;
  const nes = new NES({
    emulateSound: true,
    sampleRate: RATE,
    onFrame: (buffer) => {
      frameBuffer = Uint32Array.from(buffer);
    },
    onAudioSample: (left) => samples.push(left),
  });
  nes.loadROM(fs.readFileSync(romPath));

  const ram = (address) => nes.mmap.load(address);
  const counter = () => ram(0x12) | (ram(0x13) << 8);
  const hash = (buffer) => {
    let value = 0;
    for (const pixel of buffer) value = (value * 31 + pixel) >>> 0;
    return value;
  };

  // The first note, for the pitch check.
  for (let i = 0; i < NOTE_FRAMES; i++) nes.frame();
  const noteSamples = samples.slice();
  const noteHz = dominantHz(noteSamples, RATE);
  const counterAfterNote = counter();

  // The rest of the run, for the picture and the counters.
  for (let i = NOTE_FRAMES; i < frames; i++) nes.frame();

  const colours = new Map();
  for (const pixel of frameBuffer) {
    const key = `${(pixel >> 16) & 0xff},${(pixel >> 8) & 0xff},${pixel & 0xff}`;
    colours.set(key, (colours.get(key) ?? 0) + 1);
  }
  const ranked = [...colours.entries()].sort((a, b) => b[1] - a[1]);

  // The D-pad: hold Right and watch the byte the ROM reads back, and whether
  // the picture changes (the ball moves).
  const before = hash(frameBuffer);
  nes.buttonDown(1, Controller.BUTTON_RIGHT);
  for (let i = 0; i < 20; i++) nes.frame();
  const joy = ram(0x10);
  const moved = hash(frameBuffer) !== before;
  nes.buttonUp(1, Controller.BUTTON_RIGHT);

  console.log(
    JSON.stringify({
      frames: counter(),
      frames_after_note: counterAfterNote,
      colours: colours.size,
      common_pixels: ranked.length ? ranked[0][1] : 0,
      total_pixels: frameBuffer.length,
      audio_samples: samples.length,
      peak: peak(noteSamples),
      note_hz: noteHz,
      joy,
      moved,
    }),
  );
}

main().catch((error) => {
  console.error(String(error && error.stack ? error.stack : error));
  process.exit(1);
});
