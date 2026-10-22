# Web View Test

The test client of webd's full views (docs/webd-plan.md, P2 slice 3). It opens one window
showing view A, a browser view, and runs a fixed scenario against the harness server (the
first `http://` argument, or `http://10.0.2.2:18081`): a content-blocking rule list,
navigation and history, the cookies of the browser profile and of the app's own profile,
a download, JavaScript dialogs, a popup request, a held `mailto:` navigation, what a view
without the network scope may not do, scroll deltas and frame latency.

Each step prints one line `WEBDBROWSE:<STEP>:PASS:<detail>` or `WEBDBROWSE:<STEP>:FAIL:<why>`,
and the last one `WEBDBROWSE:DONE:PASS` or `WEBDBROWSE:DONE:FAIL`. The status line under the
page shows the last of them.

Ctrl+Q quits; every other key, the pointer and the wheel go to the page.

It ships only in images built with `LAZYOS_WEBDDEMO=1` (`python tools/run_demo.py --webd-demo`).
