fn main() {
    // Framework-dependent deployment: stages the Windows App Runtime bootstrap
    // DLL next to the executable. The runtime itself must be installed on the
    // machine (Windows App Runtime 2.x); `bootstrap()` in main prompts for it
    // when it is missing.
    windows_reactor_setup::as_framework_dependent();
}
