// The inputd flood (U2, review of #659 H3): fill inputd's shared service
// endpoint, the queue every client's calls (and, before the fix, the
// compositor's focus changes) wait in, with one-way messages for the given
// number of seconds (first argument, default 30). inputd drops them all; the
// point is the full queue. Prints ACCT:FLOOD:sent=<n>:full=<m>, where `full`
// counts the sends the kernel refused because the queue was full: the
// harness (prompt_judge.py) needs it above zero to call the flood real.
let args = os::args();
let seconds = if args.len() > 0 { parse_int(args[0]) } else { 30 };
let input = msg::connect("os.lazy.input.v1", "os.lazy.input.v1");
let sent = 0;
let full = 0;
let deadline = os::clock() + seconds;
while os::clock() < deadline {
    for i in 0..200 {
        try {
            input.invoke_oneway("TextInput", ["x"]);
            sent += 1;
        } catch (e) {
            full += 1;
        }
    }
}
print("ACCT:FLOOD:sent=" + sent + ":full=" + full);
