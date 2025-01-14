use anyhow::Result;
use clap::Parser;
use log::{info, LevelFilter};
use rs1541fs::cbm::Cbm;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Device number to test (8-15)
    #[arg(short, long, default_value_t = 8)]
    device: u8,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Setup logging
    env_logger::builder()
        .filter_level(if args.verbose {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        })
        .init();

    info!("OpenCBM Test Application");

    // Create CBM interface
    let mut cbm = Cbm::new()?;

    // Setup command line editor
    let mut rl = DefaultEditor::new()?;

    loop {
        let readline = rl.readline("test_opencbm> ");
        match readline {
            Ok(line) => {
                rl.add_history_entry(line.as_str())?;

                let cmd: Vec<&str> = line.split_whitespace().collect();
                if cmd.is_empty() {
                    continue;
                }

                match cmd[0] {
                    "quit" | "exit" | "q" | "x" => break,

                    "identify" | "id" | "i" => match cbm.identify(args.device) {
                        Ok(info) => println!("Device info: {:?}", info),
                        Err(e) => println!("Error: {}", e),
                    },

                    "status" | "getstatus" | "s" => match cbm.get_status(args.device) {
                        Ok(status) => println!("Status: {}", status),
                        Err(e) => println!("Error: {}", e),
                    },

                    "reset" | "resetbus" | "busreset" | "r" | "b" => match cbm.reset_bus() {
                        Ok(()) => println!("Bus reset complete"),
                        Err(e) => println!("Error: {}", e),
                    },

                    "u" | "usbreset" | "resetusb" => match cbm.blocking_usb_reset() {
                        Ok(()) => println!("USB reset complete"),
                        Err(e) => println!("Error: {}", e),
                    },

                    "command" | "cmd" | "c" => {
                        if cmd.len() < 2 {
                            println!("Usage: command <cmd-string>");
                            continue;
                        }
                        let cmd_str = cmd[1..].join(" ");
                        match cbm.send_command(args.device, &cmd_str) {
                            Ok(()) => {
                                println!("Command sent successfully");
                                // Get status after command
                                if let Ok(status) = cbm.get_status(args.device) {
                                    println!("Status: {}", status);
                                }
                            }
                            Err(e) => println!("Error: {}", e),
                        }
                    }

                    "format" | "f" => {
                        if cmd.len() != 3 {
                            println!("Usage: format <name> <id>");
                            continue;
                        }
                        match cbm.format_disk(args.device, cmd[1], cmd[2]) {
                            Ok(()) => println!("Format complete"),
                            Err(e) => println!("Error: {}", e),
                        }
                    }

                    "help" | "h" | "?" => {
                        println!("Available commands:");
                        println!("  i|id|identify       - Get device info");
                        println!("  s|status            - Get device status");
                        println!("  r|b|reset           - Reset the IEC bus");
                        println!("  u|usbreset          - Reset the USB device");
                        println!("  c|command <cmd>     - Send command to device");
                        println!("  f|ormat <name> <id> - Format disk");
                        println!("  h|?|help            - Show this help");
                        println!("  q|x|quit|exit       - Exit program");
                    }

                    _ => println!("Unknown command. Type 'help' for available commands."),
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C");
                break;
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }

    Ok(())
}
