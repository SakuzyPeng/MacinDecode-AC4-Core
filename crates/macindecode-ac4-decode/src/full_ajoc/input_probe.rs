//! 仅供测试的输入边界观察器；不进入产品构建或公开 API。
//! 记录路由前的真实 A-SPX 结果和矩阵入口实际消费的值，不重建替代输出。
extern crate std;

use crate::{
    ajoc::{Ajoc, AjocObjectControl, AjocObjectMatrix},
    aspx::{
        qmf::QmfSlot,
        syntax::{AspxConfig, AspxData},
    },
    element_drive::QmfChannelFrame,
    var_element::VarChannelElement,
};
use std::{cell::RefCell, format, string::String, vec::Vec};

#[derive(Default)]
pub(crate) struct Observation {
    pub signals: Vec<(usize, Vec<QmfSlot>)>,
    pub applied: Option<String>,
    pub consumed: Option<(String, u8, Vec<QmfChannelFrame>)>,
}

std::thread_local! {
    static ACTIVE: RefCell<Option<Observation>> = const { RefCell::new(None) };
}

pub(crate) fn begin() {
    ACTIVE.with(|cell| {
        assert!(cell.borrow().is_none());
        *cell.borrow_mut() = Some(Observation::default());
    });
}

pub(crate) fn take() -> Observation {
    ACTIVE.with(|cell| cell.borrow_mut().take().expect("边界观察必须显式启用"))
}

pub(crate) fn signal(source: usize, values: &[QmfSlot]) {
    ACTIVE.with(|cell| {
        if let Some(record) = cell.borrow_mut().as_mut() {
            record.signals.push((source, values.to_vec()));
        }
    });
}

pub(crate) fn aspx_identity(
    element: &VarChannelElement,
    data: &[AspxData],
    config: Option<&AspxConfig>,
) -> String {
    format!("{element:?}\n{data:?}\n{config:?}\n")
}

pub(crate) fn apply(element: &VarChannelElement, data: &[AspxData], config: Option<&AspxConfig>) {
    ACTIVE.with(|cell| {
        if let Some(record) = cell.borrow_mut().as_mut() {
            assert!(record.applied.is_none());
            record.applied = Some(aspx_identity(element, data, config));
        }
    });
}

pub(crate) fn ajoc_identity(
    ajoc: &Ajoc,
    controls: &[AjocObjectControl],
    matrices: &[AjocObjectMatrix],
) -> String {
    format!("{ajoc:?}\n{controls:?}\n{matrices:?}\n")
}

pub(crate) fn consume(
    ajoc: &Ajoc,
    controls: &[AjocObjectControl],
    matrices: &[AjocObjectMatrix],
    slots: u8,
    input: &[QmfChannelFrame],
) {
    ACTIVE.with(|cell| {
        if let Some(record) = cell.borrow_mut().as_mut() {
            assert!(record.consumed.is_none());
            record.consumed = Some((
                ajoc_identity(ajoc, controls, matrices),
                slots,
                input.to_vec(),
            ));
        }
    });
}
