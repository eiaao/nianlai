//! 新版本系统通知（仅 Windows）：通知中心的“提醒”类通知，带“查看更新”“忽略此版本”两个按钮。
//! 未打包的程序要先在注册表登记应用身份（AUMID）才能发通知；失败由调用方记日志，静默回退到
//! 设置里的绿点、横幅和托盘菜单项。

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastDismissalReason, ToastDismissedEventArgs, ToastNotification,
    ToastNotificationManager,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
use windows::core::{HSTRING, Interface};

pub const AUMID: &str = "io.github.eiaao.nianlai";
const DISPLAY_NAME: &str = "拈来";
/// 提醒类通知会一直停留，到时从屏幕收起。
const ON_SCREEN: Duration = Duration::from_secs(60);
/// 通知中心副本等待用户处理的检查间隔。
const KEEP_ALIVE_POLL: Duration = Duration::from_secs(5);

/// 用户在通知上的选择，由调用方处理。
pub enum Choice {
    View,
    Ignore(String),
}

fn register() -> windows::core::Result<()> {
    let key = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{AUMID}"));
    let name: Vec<u16> = DISPLAY_NAME.encode_utf16().chain([0]).collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &key,
            &HSTRING::from("DisplayName"),
            REG_SZ.0,
            Some(name.as_ptr().cast()),
            (name.len() * 2) as u32,
        )
        .ok()
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml(version: &str) -> String {
    format!(
        r#"<toast scenario="reminder" launch="view"><visual><binding template="ToastGeneric"><text>拈来</text><text>{body}</text></binding></visual><actions><action content="查看更新" arguments="view" activationType="foreground"/><action content="忽略此版本" arguments="ignore" activationType="foreground"/></actions><audio src="ms-winsoundevent:Notification.Default"/></toast>"#,
        body = escape(&format!("发现新版本 {version}"))
    )
}

type OnChoice = Arc<dyn Fn(Choice) + Send + Sync>;

/// 创建通知并挂上按钮回调；用户点过按钮或手动关闭后 `handled` 置位。
fn create(
    version: &str,
    on_choice: &OnChoice,
    handled: &Arc<AtomicBool>,
) -> windows::core::Result<ToastNotification> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml(version)))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    let (version, on_choice, done) = (version.to_owned(), on_choice.clone(), handled.clone());
    toast.Activated(&TypedEventHandler::<
        ToastNotification,
        windows::core::IInspectable,
    >::new(move |_, args| {
        done.store(true, Ordering::Release);
        let arguments = args
            .as_ref()
            .and_then(|args| args.cast::<ToastActivatedEventArgs>().ok())
            .and_then(|args| args.Arguments().ok());
        on_choice(if arguments.is_some_and(|text| text == "ignore") {
            Choice::Ignore(version.clone())
        } else {
            Choice::View
        });
        Ok(())
    }))?;
    let done = handled.clone();
    toast.Dismissed(&TypedEventHandler::<
        ToastNotification,
        ToastDismissedEventArgs,
    >::new(move |_, args| {
        if args.as_ref().and_then(|args| args.Reason().ok())
            == Some(ToastDismissalReason::UserCanceled)
        {
            done.store(true, Ordering::Release);
        }
        Ok(())
    }))?;
    Ok(toast)
}

/// 发出新版本通知；`on_choice` 在通知线程上调用。
/// 提醒类通知会一直停留：60 秒后从屏幕收起（收起会连同通知中心记录一起删除），
/// 用户尚未处理时再静默放一份到通知中心，并持有它直到用户处理——按钮回调挂在通知对象上，
/// 没有注册 COM 激活器时对象释放后，从通知中心点击不会回到本进程。
pub fn show_update(
    version: &str,
    on_choice: impl Fn(Choice) + Send + Sync + 'static,
) -> Result<(), String> {
    let fail = |error: windows::core::Error| format!("{error}");
    register().map_err(fail)?;
    let on_choice: OnChoice = Arc::new(on_choice);
    let handled = Arc::new(AtomicBool::new(false));
    let toast = create(version, &on_choice, &handled).map_err(fail)?;
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID)).map_err(fail)?;
    notifier.Show(&toast).map_err(fail)?;
    let version = version.to_owned();
    std::thread::Builder::new()
        .name("update-toast".into())
        .spawn(move || {
            std::thread::sleep(ON_SCREEN);
            if handled.load(Ordering::Acquire) {
                return;
            }
            let _ = notifier.Hide(&toast);
            let quiet = create(&version, &on_choice, &handled).and_then(|copy| {
                copy.SetSuppressPopup(true)?;
                notifier.Show(&copy)?;
                Ok(copy)
            });
            if let Ok(_copy) = quiet {
                while !handled.load(Ordering::Acquire) {
                    std::thread::sleep(KEEP_ALIVE_POLL);
                }
            }
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_xml_is_a_reminder_with_two_buttons_and_escaped_text() {
        let text = xml("1.0.0-rc.1 <&>");
        assert!(text.contains(r#"scenario="reminder""#));
        assert!(text.contains(r#"arguments="view""#));
        assert!(text.contains(r#"arguments="ignore""#));
        assert!(text.contains("发现新版本 1.0.0-rc.1 &lt;&amp;&gt;"));
        assert!(!text.contains("<&>"));
    }

    #[test]
    fn app_identity_matches_the_release_contract() {
        assert_eq!(AUMID, "io.github.eiaao.nianlai");
        assert_eq!(DISPLAY_NAME, "拈来");
    }
}
