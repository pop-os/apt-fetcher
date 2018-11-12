use std::io;
use std::process::Command;
use status::StatusExt;

/// Upgrades all packages, ensuring the the core packages are retained.
pub fn upgrade<I, S>(core_packages: I) -> io::Result<()>
    where I: Iterator<Item = S>,
          S: AsRef<str>
{
    install_core_packages(core_packages)
        .and_then(|_| upgrade_packages())
}

fn upgrade_packages() -> io::Result<()> {
    Command::new("aptitude")
        .args(&["upgrade", "-y"])
        .status()
        .and_then(StatusExt::as_result)
}

fn install_core_packages<I, S>(core_packages: I) -> io::Result<()>
    where I: Iterator<Item = S>,
          S: AsRef<str>
{
    let mut command = Command::new("aptitude");
    command.args(&["install", "-y"]);

    for argument in core_packages {
        command.arg(argument.as_ref());
    }

    command.status().and_then(StatusExt::as_result)
}
