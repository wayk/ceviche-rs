use std::env;
use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

use ctrlc;
use log::info;

use crate::controller::{ControllerInterface, ServiceMainFn};
use crate::session;
use crate::Error;
use crate::ServiceEvent;

type MacosServiceMainWrapperFn = extern "system" fn(args: Vec<String>);
pub type Session = session::Session_<u32>;

pub enum LaunchAgentTargetSesssion {
    GUI,
    NonGUI,
    PerUser,
    PreLogin,
}

impl fmt::Display for LaunchAgentTargetSesssion {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LaunchAgentTargetSesssion::GUI => write!(f, "Aqua"),
            LaunchAgentTargetSesssion::NonGUI => write!(f, "StandardIO"),
            LaunchAgentTargetSesssion::PerUser => write!(f, "Background"),
            LaunchAgentTargetSesssion::PreLogin => write!(f, "LoginWindow"),
        }
    }
}

fn launchctl_load_daemon(plist_path: &Path) -> Result<(), Error> {
    let output = Command::new("launchctl")
        .arg("load")
        .arg(&plist_path.to_str().unwrap())
        .output()
        .map_err(|e| {
            Error::new(&format!(
                "Failed to load plist {}: {}",
                plist_path.display(),
                e
            ))
        })?;
    if output.stdout.len() > 0 {
        info!("{}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(())
}

fn launchctl_unload_daemon(plist_path: &Path) -> Result<(), Error> {
    let output = Command::new("launchctl")
        .arg("unload")
        .arg(&plist_path.to_str().unwrap())
        .output()
        .map_err(|e| {
            Error::new(&format!(
                "Failed to unload plist {}: {}",
                plist_path.display(),
                e
            ))
        })?;
    if output.stdout.len() > 0 {
        info!("{}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(())
}

fn launchctl_start_daemon(name: &str) -> Result<(), Error> {
    let output = Command::new("launchctl")
        .arg("start")
        .arg(name)
        .output()
        .map_err(|e| Error::new(&format!("Failed to start {}: {}", name, e)))?;
    if output.stdout.len() > 0 {
        info!("{}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(())
}

fn launchctl_stop_daemon(name: &str) -> Result<(), Error> {
    let output = Command::new("launchctl")
        .arg("stop")
        .arg(name)
        .output()
        .map_err(|e| Error::new(&format!("Failed to stop {}: {}", name, e)))?;
    if output.stdout.len() > 0 {
        info!("{}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(())
}

pub struct MacosController {
    /// Manages the service on the system.
    pub service_name: String,
    pub display_name: String,
    pub description: String,
    pub is_agent: bool,
    pub session_types: Option<Vec<LaunchAgentTargetSesssion>>,
    pub keep_alive: bool,
}

impl MacosController {
    pub fn new(service_name: &str, display_name: &str, description: &str) -> MacosController {
        MacosController {
            service_name: service_name.to_string(),
            display_name: display_name.to_string(),
            description: description.to_string(),
            is_agent: false,
            session_types: None,
            keep_alive: true,
        }
    }

    /// Register the `service_main_wrapper` function, this function is generated by the `Service!` macro.
    pub fn register(
        &mut self,
        service_main_wrapper: MacosServiceMainWrapperFn,
    ) -> Result<(), Error> {
        service_main_wrapper(env::args().collect());
        Ok(())
    }

    fn get_plist_content(&self) -> Result<String, Error> {
        let mut current_exe = env::current_exe()
            .map_err(|e| Error::new(&format!("env::current_exe() failed: {}", e)))?;
        let current_exe_str = current_exe
            .to_str().expect("current_exe path to be unicode").to_string();

        current_exe.pop();
        let working_dir_str = current_exe
            .to_str().expect("working_dir path to be unicode");

        let mut plist = String::new();
        plist.push_str(r#"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>"#);

        plist.push_str(&format!(r#"
<key>Disabled</key>
<false/>
<key>Label</key>
<string>{}</string>
<key>ProgramArguments</key>
<array>
<string>{}</string>
</array>
<key>WorkingDirectory</key>
<string>{}</string>
<key>RunAtLoad</key>
<true/>"#,
            self.service_name,
            current_exe_str,
            working_dir_str,
        ));

        if self.is_agent {
            if let Some(session_types) = self.session_types.as_ref() {
                plist.push_str(r#"
<key>LimitLoadToSessionType</key>
<array>"#);

                for session_type in session_types {
                    plist.push_str(&format!(r#"
<string>{}</string>"#, session_type));
                }

                plist.push_str(r#"
</array>"#);
            }
        }

        if self.keep_alive {
            plist.push_str(r#"
<key>KeepAlive</key>
<true/>"#);
        }

        plist.push_str(r#"
</dict>
</plist>"#);

        Ok(plist)
    }

    fn write_plist(&self, path: &Path) -> Result<(), Error> {
        info!("Writing plist file {}", path.display());
        let content = self.get_plist_content()?;
        File::create(path)
            .and_then(|mut file| file.write_all(content.as_bytes()))
            .map_err(|e| Error::new(&format!("Failed to write {}: {}", path.display(), e)))

    }

    fn plist_path(&mut self) -> PathBuf {
        Path::new("/Library/")
        .join(if self.is_agent { "LaunchAgents/" } else { "LaunchDaemons/"})
        .join(format!("{}.plist", &self.service_name))
    }
}

impl ControllerInterface for MacosController {
    /// Creates the service on the system.
    fn create(&mut self) -> Result<(), Error> {
        let plist_path = self.plist_path();
            
        self.write_plist(&plist_path)?;
        if !self.is_agent {
            return launchctl_load_daemon(&plist_path)
        }
        Ok(())
    }
    /// Deletes the service.
    fn delete(&mut self) -> Result<(), Error> {
        let plist_path = self.plist_path();
        if !self.is_agent {
            launchctl_unload_daemon(&plist_path)?;
        }
        fs::remove_file(&plist_path)
            .map_err(|e| Error::new(&format!("Failed to delete {}: {}", plist_path.display(), e)))
    }
    /// Starts the service.
    fn start(&mut self) -> Result<(), Error> {
        launchctl_start_daemon(&self.service_name)
    }
    /// Stops the service.
    fn stop(&mut self) -> Result<(), Error> {
        launchctl_stop_daemon(&self.service_name)
    }
    // Loads the agent service.
    fn load(&mut self) -> Result<(), Error> {
        launchctl_load_daemon(&self.plist_path())
    }
    // Loads the agent service.
    fn unload(&mut self) -> Result<(), Error> {
        launchctl_unload_daemon(&self.plist_path())
    }
}

/// Generates a `service_main_wrapper` that wraps the provided service main function.
#[macro_export]
macro_rules! Service {
    ($name:expr, $function:ident) => {
        extern "system" fn service_main_wrapper(args: Vec<String>) {
            dispatch($function, args);
        }
    };
}

#[doc(hidden)]
pub fn dispatch<T: Send + 'static>(service_main: ServiceMainFn<T>, args: Vec<String>) {
    let (tx, rx) = mpsc::channel();
    let _tx = tx.clone();

    ctrlc::set_handler(move || {
        let _ = tx.send(ServiceEvent::Stop);
    }).expect("Failed to register Ctrl-C handler");
    service_main(rx, _tx, args, false);
}