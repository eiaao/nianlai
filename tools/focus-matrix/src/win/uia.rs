use super::{hwnd, safety::Control};
use crate::{protocol::normalize, scenario::Control as Spec};
use std::{
    collections::BTreeMap,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{System::Com::*, UI::Accessibility::*},
    core::Interface,
};

#[derive(Debug, Clone)]
pub struct Element {
    pub id: u64,
    pub class: String,
    pub kind: i32,
    pub rect: [i32; 4],
    pub text: Option<String>,
    pub selection: Option<String>,
}
enum Request {
    Focused {
        root: isize,
        spec: Spec,
    },
    Find {
        root: isize,
        name: Option<String>,
        class: Option<String>,
        kind: Option<i32>,
    },
    Read(u64),
    Focus(u64),
    Compare(u64, u64),
}
enum Response {
    Element(Element),
    Done,
    Same(bool),
}
struct Message {
    request: Request,
    reply: mpsc::SyncSender<Result<Response, String>>,
}
pub struct Uia {
    send: mpsc::Sender<Message>,
    control: Arc<Control>,
}
impl Uia {
    pub fn new(control: Arc<Control>) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Message>();
        let (ready, receive) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("focus-matrix-uia".into())
            .spawn(move || unsafe {
                if let Err(e) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
                    let _ = ready.send(Err(e.to_string()));
                    return;
                }
                let automation: Result<IUIAutomation, _> =
                    CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER);
                let Ok(automation) = automation else {
                    let _ = ready.send(Err("UIA 初始化失败".into()));
                    CoUninitialize();
                    return;
                };
                if let Ok(options) = automation.cast::<IUIAutomation2>() {
                    let _ = options.SetConnectionTimeout(300);
                    let _ = options.SetTransactionTimeout(300);
                }
                let _ = ready.send(Ok(()));
                let mut elements = BTreeMap::<u64, IUIAutomationElement>::new();
                let mut next = 1;
                while let Ok(message) = rx.recv() {
                    let result = (|| -> Result<Response, String> {
                        match message.request {
                            Request::Focus(id) => {
                                elements
                                    .get(&id)
                                    .ok_or("UIA 元素已失效")?
                                    .SetFocus()
                                    .map_err(|e| e.to_string())?;
                                Ok(Response::Done)
                            }
                            Request::Compare(a, b) => Ok(Response::Same(
                                automation
                                    .CompareElements(
                                        elements.get(&a).ok_or("缺少原元素")?,
                                        elements.get(&b).ok_or("缺少现元素")?,
                                    )
                                    .map_err(|e| e.to_string())?
                                    .as_bool(),
                            )),
                            Request::Read(id) => {
                                let element = elements.get(&id).ok_or("UIA 元素已失效")?;
                                snapshot(element, id).map(Response::Element)
                            }
                            Request::Focused { root, spec } => {
                                let element =
                                    automation.GetFocusedElement().map_err(|e| e.to_string())?;
                                if !belongs(&automation, &element, root)? {
                                    return Err("UIA 焦点不在已认领窗口内".into());
                                }
                                let snap = snapshot(&element, next)?;
                                if spec.class.as_ref().is_some_and(|s| s != &snap.class)
                                    || spec.control_type.is_some_and(|k| k != snap.kind)
                                {
                                    return Err(format!(
                                        "UIA 控件条件不匹配：类 {}，类型 {}",
                                        snap.class, snap.kind
                                    ));
                                }
                                elements.insert(next, element);
                                next += 1;
                                Ok(Response::Element(snap))
                            }
                            Request::Find {
                                root,
                                name,
                                class,
                                kind,
                            } => {
                                let root = automation
                                    .ElementFromHandle(hwnd(root))
                                    .map_err(|e| e.to_string())?;
                                let walker =
                                    automation.ControlViewWalker().map_err(|e| e.to_string())?;
                                let mut stack = vec![root];
                                let start = Instant::now();
                                let mut count = 0;
                                while let Some(element) = stack.pop() {
                                    count += 1;
                                    if count > 2000 || start.elapsed() > Duration::from_millis(700)
                                    {
                                        return Err("UIA 树搜索超过预算".into());
                                    }
                                    if class.as_ref().is_none_or(|v| {
                                        element.CurrentClassName().is_ok_and(|s| s == v.as_str())
                                    }) && name.as_ref().is_none_or(|v| {
                                        element.CurrentName().is_ok_and(|s| s == v.as_str())
                                    }) && kind.is_none_or(|v| {
                                        element.CurrentControlType().is_ok_and(|k| k.0 == v)
                                    }) {
                                        let snap = snapshot(&element, next)?;
                                        elements.insert(next, element);
                                        next += 1;
                                        return Ok(Response::Element(snap));
                                    }
                                    if let Ok(child) = walker.GetFirstChildElement(&element) {
                                        stack.push(child.clone());
                                        let mut cur = child;
                                        for _ in 0..200 {
                                            let Ok(sibling) = walker.GetNextSiblingElement(&cur)
                                            else {
                                                break;
                                            };
                                            stack.push(sibling.clone());
                                            cur = sibling;
                                        }
                                    }
                                }
                                Err("未找到指定 UIA 控件".into())
                            }
                        }
                    })();
                    let _ = message.reply.send(result);
                    // 只保留最近的试验对象；对已淘汰 ID 的访问会明确失败。
                    while elements.len() > 512 {
                        if let Some(id) = elements.keys().next().copied() {
                            elements.remove(&id);
                        }
                    }
                }
                drop(elements);
                drop(automation);
                CoUninitialize();
            })
            .map_err(|e| e.to_string())?;
        receive
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "UIA 初始化超时")??;
        Ok(Self { send: tx, control })
    }
    fn call(&self, request: Request) -> Result<Response, String> {
        self.control.check()?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.send
            .send(Message { request, reply: tx })
            .map_err(|_| "UIA 工作线程已退出")?;
        let start = Instant::now();
        loop {
            self.control.check()?;
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(v) => return v,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("UIA 工作线程断开".into()),
                Err(_) if start.elapsed() > Duration::from_millis(1200) => {
                    self.control.stop("UIA 调用超过硬预算；停止继续注入");
                    return Err("UIA 超时".into());
                }
                Err(_) => {}
            }
        }
    }
    pub fn focused(&self, root: isize, spec: &Spec) -> Result<Element, String> {
        match self.call(Request::Focused {
            root,
            spec: spec.clone(),
        })? {
            Response::Element(e) => Ok(e),
            _ => Err("UIA 响应错误".into()),
        }
    }
    pub fn find(
        &self,
        root: isize,
        name: Option<&str>,
        class: Option<&str>,
        kind: Option<i32>,
    ) -> Result<Element, String> {
        match self.call(Request::Find {
            root,
            name: name.map(str::to_owned),
            class: class.map(str::to_owned),
            kind,
        })? {
            Response::Element(e) => Ok(e),
            _ => Err("UIA 响应错误".into()),
        }
    }
    pub fn read(&self, id: u64) -> Result<Element, String> {
        match self.call(Request::Read(id))? {
            Response::Element(e) => Ok(e),
            _ => Err("UIA 响应错误".into()),
        }
    }
    pub fn focus(&self, id: u64) -> Result<(), String> {
        self.call(Request::Focus(id)).map(|_| ())
    }
    pub fn same(&self, a: u64, b: u64) -> bool {
        matches!(self.call(Request::Compare(a, b)), Ok(Response::Same(true)))
    }
}
unsafe fn belongs(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
    root: isize,
) -> Result<bool, String> {
    unsafe {
        let expected = automation
            .ElementFromHandle(hwnd(root))
            .map_err(|e| e.to_string())?;
        let walker = automation.RawViewWalker().map_err(|e| e.to_string())?;
        let mut cur = element.clone();
        for _ in 0..64 {
            if automation
                .CompareElements(&cur, &expected)
                .is_ok_and(|b| b.as_bool())
            {
                return Ok(true);
            }
            let Ok(parent) = walker.GetParentElement(&cur) else {
                break;
            };
            cur = parent;
        }
        Ok(false)
    }
}
unsafe fn snapshot(element: &IUIAutomationElement, id: u64) -> Result<Element, String> {
    unsafe {
        if element
            .CurrentIsPassword()
            .map_err(|e| e.to_string())?
            .as_bool()
        {
            return Err("拒绝读取密码控件".into());
        }
        let class = element
            .CurrentClassName()
            .map_err(|e| e.to_string())?
            .to_string();
        let kind = element.CurrentControlType().map_err(|e| e.to_string())?.0;
        let rect = element.CurrentBoundingRectangle().unwrap_or_default();
        let mut text = None;
        let mut selection = None;
        if let Ok(value) =
            element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            && let Ok(v) = value.CurrentValue()
        {
            if v.len() > 200000 {
                return Err("UIA 文本超过上限".into());
            }
            text = Some(normalize(&v.to_string()));
        }
        if let Ok(pattern) =
            element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
        {
            if text.is_none()
                && let Ok(range) = pattern.DocumentRange()
                && let Ok(v) = range.GetText(200001)
            {
                if v.len() > 200000 {
                    return Err("UIA 文本超过上限".into());
                }
                text = Some(normalize(&v.to_string()));
            }
            if let Ok(ranges) = pattern.GetSelection()
                && ranges.Length().unwrap_or(0) == 1
                && let Ok(range) = ranges.GetElement(0)
                && let Ok(v) = range.GetText(200001)
            {
                selection = Some(normalize(&v.to_string()));
            }
        }
        Ok(Element {
            id,
            class,
            kind,
            rect: [rect.left, rect.top, rect.right, rect.bottom],
            text,
            selection,
        })
    }
}
