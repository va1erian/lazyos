// The prompt scenario (U2): open the Counter while the trusted prompt is up;
// a new window takes the focus, and must still show nothing above the prompt.
try {
    sys::init::launch("os.lazy.counter", "", 0);
    print("ACCT:PROMPT:WINDOW");
} catch (e) {
    print("ACCT:PROMPT:NOWINDOW");
}
