fn main() {
    // Embed the icon into the Windows executable.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Pulse");
        res.set("FileDescription", "Pulse - YouTube audio/video player");
        let _ = res.compile();
    }
}
