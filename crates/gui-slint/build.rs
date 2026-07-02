fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.compile()?;
    }

    Ok(())
}
