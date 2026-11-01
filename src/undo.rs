//! 撤回栈：记录对文档的每一次可撤回操作（画一笔 / 擦掉若干笔 / 清空图层）。
//!
//! 设计要点：
//! - 只存**差异**（被加或被删的笔迹），不存整份文档快照，内存与图层数无关；
//! - 图层用稳定 `id` 定位，而不是索引 —— 增删、移动图层后旧记录依然指向正确的层；
//! - 上限 [`UNDO_LIMIT`] 条，超出后丢弃最早的记录。

use std::collections::VecDeque;

use crate::model::{Document, Stroke};

/// 最多可撤回的操作数（每画一笔算一条）。
pub const UNDO_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// 新增了一笔：撤回时把它删掉。
    Added {
        layer: u64,
        stroke: Stroke,
        /// 插入时的位置，删除时优先按它定位。
        index: usize,
    },
    /// 删除了一笔或多笔：撤回时按原始位置放回去（保持叠放顺序）。
    Removed {
        layer: u64,
        strokes: Vec<(usize, Stroke)>,
    },
}

/// 一次撤回实际影响到的图层索引，用于决定重绘范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UndoResult {
    pub layer: usize,
    /// 是否恢复了被删掉的笔迹（这种改动可能覆盖很大区域）。
    pub restored: bool,
}

#[derive(Debug, Default)]
pub struct UndoStack {
    entries: VecDeque<Edit>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 记录一次操作；超过上限时丢弃最早的一条。
    pub fn record(&mut self, edit: Edit) {
        if let Edit::Removed { strokes, .. } = &edit {
            // 什么都没删掉的操作不值得占用历史。
            if strokes.is_empty() {
                return;
            }
        }
        self.entries.push_back(edit);
        while self.entries.len() > UNDO_LIMIT {
            self.entries.pop_front();
        }
    }

    /// 撤回一步。返回 `None` 表示没有可撤回的内容。
    ///
    /// 若某条记录指向的图层已被删除、或那一笔已被擦掉，就跳过继续往前找，
    /// 保证 Ctrl+Z 要么真的删掉一笔，要么明确地什么都不做。
    pub fn undo(&mut self, document: &mut Document) -> Option<UndoResult> {
        while let Some(edit) = self.entries.pop_back() {
            match edit {
                Edit::Added {
                    layer,
                    stroke,
                    index,
                } => {
                    let Some(layer_index) = document.layer_index_by_id(layer) else {
                        continue;
                    };
                    let strokes = &mut document.layers[layer_index].strokes;
                    let position = if strokes.get(index) == Some(&stroke) {
                        Some(index)
                    } else {
                        strokes.iter().rposition(|candidate| candidate == &stroke)
                    };
                    if let Some(position) = position {
                        strokes.remove(position);
                        return Some(UndoResult {
                            layer: layer_index,
                            restored: false,
                        });
                    }
                }
                Edit::Removed { layer, strokes } => {
                    let Some(layer_index) = document.layer_index_by_id(layer) else {
                        continue;
                    };
                    let target = &mut document.layers[layer_index].strokes;
                    // 按原始索引升序插回，位置关系保持原样。
                    let mut restored: Vec<_> = strokes;
                    restored.sort_by_key(|(index, _)| *index);
                    for (index, stroke) in restored {
                        let position = index.min(target.len());
                        target.insert(position, stroke);
                    }
                    return Some(UndoResult {
                        layer: layer_index,
                        restored: true,
                    });
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Point, Rgba};

    fn stroke_at(x: f64) -> Stroke {
        Stroke::constant(
            Rgba::new(0, 0, 0, 255),
            2.0,
            vec![Point::new(x, 0.0), Point::new(x + 10.0, 10.0)],
        )
    }

    fn document_with(strokes: Vec<Stroke>) -> Document {
        let mut document = Document::new(800.0, 600.0);
        document.layers[0].strokes = strokes;
        document
    }

    #[test]
    fn undo_removes_the_most_recent_stroke_only() {
        let mut document = document_with(vec![stroke_at(0.0), stroke_at(20.0)]);
        let layer = document.layers[0].id;
        let mut stack = UndoStack::new();
        stack.record(Edit::Added {
            layer,
            stroke: stroke_at(20.0),
            index: 1,
        });

        let result = stack.undo(&mut document).expect("应能撤回");
        assert_eq!(result.layer, 0);
        assert!(!result.restored);
        assert_eq!(document.layers[0].strokes, vec![stroke_at(0.0)]);
        assert!(stack.undo(&mut document).is_none(), "只有一条记录");
    }

    #[test]
    fn history_is_capped_at_undo_limit() {
        let mut stack = UndoStack::new();
        for index in 0..(UNDO_LIMIT + 20) {
            stack.record(Edit::Added {
                layer: 1,
                stroke: stroke_at(index as f64),
                index,
            });
        }
        assert_eq!(stack.len(), UNDO_LIMIT);
    }

    #[test]
    fn undo_skips_records_for_deleted_layers() {
        let mut document = document_with(vec![stroke_at(0.0)]);
        let mut stack = UndoStack::new();
        // 一条指向不存在图层的记录 + 一条有效的
        stack.record(Edit::Added {
            layer: 999,
            stroke: stroke_at(50.0),
            index: 0,
        });
        stack.record(Edit::Added {
            layer: document.layers[0].id,
            stroke: stroke_at(0.0),
            index: 0,
        });

        assert!(stack.undo(&mut document).is_some());
        assert!(document.layers[0].strokes.is_empty());
        assert!(stack.undo(&mut document).is_none(), "无效记录直接丢弃");
    }

    #[test]
    fn removed_strokes_are_restored_at_original_positions() {
        let mut document = document_with(vec![stroke_at(0.0), stroke_at(60.0)]);
        let layer = document.layers[0].id;
        let middle = stroke_at(30.0);

        let mut stack = UndoStack::new();
        stack.record(Edit::Removed {
            layer,
            strokes: vec![(1, middle.clone())],
        });

        let result = stack.undo(&mut document).expect("应能恢复");
        assert!(result.restored);
        assert_eq!(
            document.layers[0].strokes,
            vec![stroke_at(0.0), middle, stroke_at(60.0)]
        );
    }

    #[test]
    fn empty_removal_is_not_recorded() {
        let mut stack = UndoStack::new();
        stack.record(Edit::Removed {
            layer: 1,
            strokes: Vec::new(),
        });
        assert!(stack.is_empty());
    }
}
