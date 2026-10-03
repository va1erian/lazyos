# LazyRAD

LazyRAD is the rapid-application-development tool of LazyOS: design forms,
write Rhai scripts for their events, press Play to try the result, and use
**File → Make LazyOS App** to turn the project into an installed app.

Projects live in `~/projects`. LazyRAD keeps its settings, and the files of
projects you run from it, in `~/.apps/os.lazy.lazyrad`.

## Making an app

Make LazyOS App checks the package, then opens the Package Installer, which
shows what the app is allowed to do (the services and topics its scripts call)
and installs it when you agree. LazyRAD never installs anything itself.

## Known limit

Play runs your project under LazyRAD's own permissions, not the ones the
installed app would get: a project that calls services LazyRAD does not declare
is refused while playing, and works once installed.
