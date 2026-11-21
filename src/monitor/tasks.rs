//! Scheduled tasks in the `\widepeek` folder of the Task Scheduler.
//!
//! `MonitorOff` and `MonitorOn` run with the highest privileges of the user,
//! so the unelevated launcher can toggle the monitor without a UAC prompt.
//! `RestoreAtLogon` runs `restore --at-logon` unelevated at each logon.
//! All tasks run the exe in `C:\Program Files\widepeek`, which only
//! administrators can change.

use std::path::Path;

use anyhow::Context;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::TaskScheduler::{
    ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::BSTR;

use crate::com::Com;

pub const MONITOR_OFF: &str = "MonitorOff";
pub const MONITOR_ON: &str = "MonitorOn";
pub const RESTORE_AT_LOGON: &str = "RestoreAtLogon";
const FOLDER: &str = r"\widepeek";

/// The connected service. `_com` must drop last, so it is the last field.
struct Scheduler {
    service: ITaskService,
    _com: Com,
}

impl Scheduler {
    fn connect() -> anyhow::Result<Self> {
        let com = Com::init()?;
        // SAFETY: standard COM object creation and connect to the local host.
        let service: ITaskService =
            unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
                .context("cannot create the Task Scheduler object")?;
        let none = VARIANT::default();
        // SAFETY: empty variants connect as the current user to this PC.
        unsafe { service.Connect(&none, &none, &none, &none) }
            .context("cannot connect to the Task Scheduler")?;
        Ok(Self { service, _com: com })
    }

    fn folder(&self) -> windows::core::Result<ITaskFolder> {
        // SAFETY: valid BSTR path.
        unsafe { self.service.GetFolder(&BSTR::from(FOLDER)) }
    }

    fn create_folder(&self) -> anyhow::Result<ITaskFolder> {
        // SAFETY: valid BSTR paths; the default security descriptor.
        unsafe {
            let root = self.service.GetFolder(&BSTR::from(r"\"))?;
            root.CreateFolder(&BSTR::from(FOLDER), &VARIANT::default())
        }
        .context("cannot create the task folder \\widepeek")
    }
}

/// Starts a task and returns at once. The caller checks the result.
pub fn run(name: &str) -> anyhow::Result<()> {
    let scheduler = Scheduler::connect()?;
    // SAFETY: valid BSTR name; no parameters.
    unsafe {
        let task = scheduler.folder()?.GetTask(&BSTR::from(name))?;
        task.Run(&VARIANT::default())?;
    }
    info!("started task {FOLDER}\\{name}");
    Ok(())
}

/// Creates the three tasks for `exe` and the user `sid`. Needs administrator
/// rights. An old `\widepeek` folder is deleted first, so its security
/// descriptor never comes from a folder that another user created.
pub fn install(exe: &Path, sid: &str) -> anyhow::Result<()> {
    uninstall().context("cannot replace the old task folder \\widepeek")?;
    let exe = exe.display().to_string();
    let scheduler = Scheduler::connect()?;
    let folder = scheduler.create_folder()?;
    let tasks = [
        (
            MONITOR_OFF,
            task_xml(&TaskSpec {
                description: "widepeek: disables the monitor device for true stretched",
                sid,
                exe: &exe,
                args: "monitor-off",
                elevated: true,
                at_logon: false,
            }),
        ),
        (
            MONITOR_ON,
            task_xml(&TaskSpec {
                description: "widepeek: enables the monitor device again",
                sid,
                exe: &exe,
                args: "monitor-on",
                elevated: true,
                at_logon: false,
            }),
        ),
        (
            RESTORE_AT_LOGON,
            task_xml(&TaskSpec {
                description: "widepeek: restores the native display after a crash or a reboot",
                sid,
                exe: &exe,
                args: "restore --at-logon",
                elevated: false,
                at_logon: true,
            }),
        ),
    ];
    let none = VARIANT::default();
    for (name, xml) in tasks {
        // SAFETY: valid BSTRs; the principal in the XML defines the user.
        unsafe {
            folder.RegisterTask(
                &BSTR::from(name),
                &BSTR::from(xml),
                TASK_CREATE_OR_UPDATE.0,
                &none,
                &none,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &none,
            )
        }
        .with_context(|| format!("cannot register the task {name}"))?;
        info!("registered task {FOLDER}\\{name}");
    }
    Ok(())
}

/// Deletes the tasks and the folder. Missing parts are fine.
pub fn uninstall() -> anyhow::Result<()> {
    let scheduler = Scheduler::connect()?;
    let Ok(folder) = scheduler.folder() else {
        return Ok(());
    };
    for name in [MONITOR_OFF, MONITOR_ON, RESTORE_AT_LOGON] {
        // SAFETY: valid BSTR name.
        if unsafe { folder.DeleteTask(&BSTR::from(name), 0) }.is_ok() {
            info!("deleted task {FOLDER}\\{name}");
        }
    }
    // SAFETY: valid BSTR paths.
    unsafe {
        let root = scheduler.service.GetFolder(&BSTR::from(r"\"))?;
        root.DeleteFolder(&BSTR::from(FOLDER), 0)
    }
    .context("cannot delete the task folder \\widepeek")
}

struct TaskSpec<'a> {
    description: &'a str,
    sid: &'a str,
    exe: &'a str,
    args: &'a str,
    elevated: bool,
    at_logon: bool,
}

fn task_xml(spec: &TaskSpec<'_>) -> String {
    let sid = escape(spec.sid);
    let triggers = if spec.at_logon {
        format!(
            "<LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId>\
             <Delay>PT5S</Delay></LogonTrigger>"
        )
    } else {
        String::new()
    };
    let run_level = if spec.elevated {
        "HighestAvailable"
    } else {
        "LeastPrivilege"
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>{description}</Description></RegistrationInfo>
  <Triggers>{triggers}</Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>{run_level}</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>Parallel</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT2M</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>"#,
        description = escape(spec.description),
        exe = escape(spec.exe),
        args = escape(spec.args),
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_elevated_task_without_trigger() {
        let xml = task_xml(&TaskSpec {
            description: "d",
            sid: "S-1-5-21-1",
            exe: r"C:\Program Files\widepeek\widepeek.exe",
            args: "monitor-off",
            elevated: true,
            at_logon: false,
        });
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<Triggers></Triggers>"));
        assert!(xml.contains(r#"<Command>"C:\Program Files\widepeek\widepeek.exe"</Command>"#));
    }

    #[test]
    fn builds_logon_task() {
        let xml = task_xml(&TaskSpec {
            description: "a & b",
            sid: "S-1-5-21-1",
            exe: "x.exe",
            args: "restore --at-logon",
            elevated: false,
            at_logon: true,
        });
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<UserId>S-1-5-21-1</UserId><Delay>PT5S</Delay>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("a &amp; b"));
    }
}
