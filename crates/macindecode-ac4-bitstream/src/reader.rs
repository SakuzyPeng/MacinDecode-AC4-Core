//! 比特流读取器。
//!
//! 规范基线 `TS103190:2025-07`。
//!
//! 输入默认不可信：所有读取都做边界检查，越界返回错误而非 panic，也不做
//! 未检查的索引。错误携带比特偏移，以便与规范条款和测试向量对应。

use core::fmt;

/// 读取失败的原因。
///
/// 此处只表达“读取本身无法完成”，不表达语义层面的合法性；后者由各语法
/// 结构自行判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// 剩余比特不足以完成本次读取。
    OutOfBounds {
        /// 请求读取的比特数。
        requested_bits: u32,
        /// 请求发生时的比特偏移。
        bit_position: u64,
        /// 该位置之后可用的比特数。
        remaining_bits: u64,
    },
    /// 请求的宽度超出单次读取上限（64 比特）。
    WidthUnsupported {
        /// 请求读取的比特数。
        requested_bits: u32,
        /// 请求发生时的比特偏移。
        bit_position: u64,
    },
    /// 变长字段的值超出当前读取 API 的目标整数范围。
    ///
    /// 既包括 [`BitReader::variable_bits`] 累加超出 `u64`，也包括其窄化或
    /// 与基值、缩放因子组合后超出目标类型。合法码流不会触发；出现即说明
    /// 输入已损坏、当前实现无法表示该值，或输入被构造用于溢出攻击。
    ValueOverflow {
        /// 请求发生时的比特偏移。
        bit_position: u64,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ReadError::OutOfBounds {
                requested_bits,
                bit_position,
                remaining_bits,
            } => write!(
                f,
                "需要 {requested_bits} 比特，但偏移 {bit_position} 处仅剩 {remaining_bits} 比特"
            ),
            ReadError::WidthUnsupported {
                requested_bits,
                bit_position,
            } => {
                write!(
                    f,
                    "偏移 {bit_position} 处请求 {requested_bits} 比特，超出 64 比特上限"
                )
            }
            ReadError::ValueOverflow { bit_position } => {
                write!(f, "偏移 {bit_position} 处变长字段数值溢出")
            }
        }
    }
}

impl core::error::Error for ReadError {}

/// 读取结果。
pub type Result<T> = core::result::Result<T, ReadError>;

/// 按比特读取的游标，最高位在前。
///
/// 读取器只在切片内前进，不会越过传入数据的边界寻找内容；access unit 的
/// 边界由调用方给定。
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    bit_position: u64,
}

impl<'a> BitReader<'a> {
    /// 在给定切片上创建读取器，起始偏移为 0。
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_position: 0,
        }
    }

    /// 当前比特偏移。
    #[must_use]
    pub const fn bit_position(&self) -> u64 {
        self.bit_position
    }

    /// 输入的总比特数。
    #[must_use]
    pub const fn total_bits(&self) -> u64 {
        // 切片长度来自内存中的实际对象，乘 8 不会溢出 u64；此处仍用饱和
        // 运算，使溢出即便发生也表现为上限而非回绕。
        (self.data.len() as u64).saturating_mul(8)
    }

    /// 当前位置之后剩余的比特数。
    #[must_use]
    pub const fn remaining_bits(&self) -> u64 {
        self.total_bits().saturating_sub(self.bit_position)
    }

    /// 当前是否位于字节边界。
    #[must_use]
    pub const fn is_byte_aligned(&self) -> bool {
        self.bit_position % 8 == 0
    }

    /// 读取 `n` 个比特，最高位在前。
    ///
    /// `n` 为 0 时返回 0 且不前进。
    ///
    /// # Errors
    ///
    /// `n` 超过 64 返回 [`ReadError::WidthUnsupported`]；剩余比特不足返回
    /// [`ReadError::OutOfBounds`]。两种情况下读取位置都保持不变。
    pub fn read_bits(&mut self, n: u32) -> Result<u64> {
        if n == 0 {
            return Ok(0);
        }
        if n > 64 {
            return Err(ReadError::WidthUnsupported {
                requested_bits: n,
                bit_position: self.bit_position,
            });
        }
        if u64::from(n) > self.remaining_bits() {
            return Err(ReadError::OutOfBounds {
                requested_bits: n,
                bit_position: self.bit_position,
                remaining_bits: self.remaining_bits(),
            });
        }

        let mut value: u64 = 0;
        for _ in 0..n {
            // 边界已在上方校验，这里的索引必然落在切片内
            let byte_index = (self.bit_position / 8) as usize;
            let bit_index = 7u32.saturating_sub((self.bit_position % 8) as u32);
            let byte = match self.data.get(byte_index) {
                Some(&byte) => byte,
                None => {
                    return Err(ReadError::OutOfBounds {
                        requested_bits: n,
                        bit_position: self.bit_position,
                        remaining_bits: 0,
                    });
                }
            };
            let bit = u64::from((byte >> bit_index) & 1);
            value = (value << 1) | bit;
            self.bit_position = self.bit_position.saturating_add(1);
        }
        Ok(value)
    }

    /// 读取一个比特并作为布尔返回。
    ///
    /// # Errors
    ///
    /// 见 [`BitReader::read_bits`]。
    pub fn read_flag(&mut self) -> Result<bool> {
        Ok(self.read_bits(1)? != 0)
    }

    /// 前进到下一个字节边界，跳过的比特被丢弃。
    ///
    /// 对应规范中的 `byte_align` 元素。
    ///
    /// # Errors
    ///
    /// 剩余比特不足以对齐时返回 [`ReadError::OutOfBounds`]。
    pub fn byte_align(&mut self) -> Result<u32> {
        let padding = match self.bit_position % 8 {
            0 => return Ok(0),
            offset => 8u32.saturating_sub(offset as u32),
        };
        self.read_bits(padding)?;
        Ok(padding)
    }

    /// 跳过 `n` 个比特。
    ///
    /// # Errors
    ///
    /// 剩余比特不足时返回 [`ReadError::OutOfBounds`]，位置保持不变。
    pub fn skip_bits(&mut self, n: u64) -> Result<()> {
        if n > self.remaining_bits() {
            return Err(ReadError::OutOfBounds {
                requested_bits: u32::try_from(n).unwrap_or(u32::MAX),
                bit_position: self.bit_position,
                remaining_bits: self.remaining_bits(),
            });
        }
        self.bit_position = self.bit_position.saturating_add(n);
        Ok(())
    }

    /// 读取变长字段 `variable_bits(n_bits)`。
    ///
    /// 对应 `TS103190-1:v1.4.1:4.2.2`（表 3）与 `4.3.2`：
    ///
    /// ```text
    /// value = 0;
    /// do {
    ///     value += read(n_bits);
    ///     if (b_read_more) {
    ///         value <<= n_bits;
    ///         value += (1 << n_bits);
    ///     }
    /// } while (b_read_more);
    /// ```
    ///
    /// 规范未给出迭代次数上限。本实现在累加溢出 `u64` 时报错而非回绕，
    /// 使损坏输入无法借该字段构造出任意大的长度值。
    ///
    /// # Errors
    ///
    /// 读取越界返回 [`ReadError::OutOfBounds`]；累加溢出返回
    /// [`ReadError::ValueOverflow`]。
    pub fn variable_bits(&mut self, n_bits: u32) -> Result<u64> {
        let start = self.bit_position;
        let mut value: u64 = 0;
        loop {
            let chunk = self.read_bits(n_bits)?;
            value = value.checked_add(chunk).ok_or(ReadError::ValueOverflow {
                bit_position: start,
            })?;

            if !self.read_flag()? {
                return Ok(value);
            }

            // 左移用乘法表达：checked_shl 只判断移位量是否超宽，不会报告
            // 高位被移出，而这里恰恰要捕获后者。
            let scale = 1u64.checked_shl(n_bits).ok_or(ReadError::ValueOverflow {
                bit_position: start,
            })?;
            value = value.checked_mul(scale).ok_or(ReadError::ValueOverflow {
                bit_position: start,
            })?;
            value = value.checked_add(scale).ok_or(ReadError::ValueOverflow {
                bit_position: start,
            })?;
        }
    }

    /// [`variable_bits`](Self::variable_bits) 的 `u32` 形式。
    ///
    /// 结果可能是长度、计数、版本或标识符；任何超出 `u32` 的值都必须报错，
    /// 不能饱和或回落为小值，否则不同码值会静默别名。
    ///
    /// # Errors
    ///
    /// 除 [`variable_bits`](Self::variable_bits) 的错误外，数值超出 `u32` 时
    /// 返回 [`ReadError::ValueOverflow`]。
    pub fn variable_bits_u32(&mut self, n_bits: u32) -> Result<u32> {
        self.variable_bits_scaled_u32(n_bits, 0, 0)
    }

    /// 读取变长扩展并计算 `base + extension * 2^shift`，全过程检查溢出。
    pub(crate) fn variable_bits_scaled_u32(
        &mut self,
        n_bits: u32,
        base: u32,
        shift: u32,
    ) -> Result<u32> {
        let start = self.bit_position;
        let extension = self.variable_bits(n_bits)?;
        let scale = 1u64.checked_shl(shift).ok_or(ReadError::ValueOverflow {
            bit_position: start,
        })?;
        let value = extension
            .checked_mul(scale)
            .and_then(|scaled| scaled.checked_add(u64::from(base)))
            .ok_or(ReadError::ValueOverflow {
                bit_position: start,
            })?;
        u32::try_from(value).map_err(|_| ReadError::ValueOverflow {
            bit_position: start,
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::unusual_byte_groupings,
    reason = "字面量按比特字段边界分组，以对应规范语法表中的字段划分"
)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_most_significant_first() {
        let mut reader = BitReader::new(&[0b1011_0010, 0b0100_1101]);
        assert_eq!(reader.read_bits(1).unwrap(), 0b1);
        assert_eq!(reader.read_bits(3).unwrap(), 0b011);
        assert_eq!(reader.read_bits(4).unwrap(), 0b0010);
        assert_eq!(reader.read_bits(8).unwrap(), 0b0100_1101);
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[test]
    fn reads_across_byte_boundary() {
        let mut reader = BitReader::new(&[0xAC, 0x40]);
        assert_eq!(reader.read_bits(16).unwrap(), 0xAC40);
    }

    #[test]
    fn zero_width_read_does_not_advance() {
        let mut reader = BitReader::new(&[0xFF]);
        assert_eq!(reader.read_bits(0).unwrap(), 0);
        assert_eq!(reader.bit_position(), 0);
    }

    #[test]
    fn out_of_bounds_leaves_position_unchanged() {
        let mut reader = BitReader::new(&[0xFF]);
        reader.read_bits(4).unwrap();
        let error = reader.read_bits(8).unwrap_err();
        assert_eq!(
            error,
            ReadError::OutOfBounds {
                requested_bits: 8,
                bit_position: 4,
                remaining_bits: 4
            }
        );
        assert_eq!(reader.bit_position(), 4, "失败的读取不得消耗比特");
    }

    #[test]
    fn rejects_width_above_64() {
        let mut reader = BitReader::new(&[0u8; 16]);
        assert!(matches!(
            reader.read_bits(65).unwrap_err(),
            ReadError::WidthUnsupported {
                requested_bits: 65,
                ..
            }
        ));
        assert_eq!(reader.bit_position(), 0);
    }

    #[test]
    fn byte_align_skips_to_boundary() {
        let mut reader = BitReader::new(&[0xFF, 0x00]);
        reader.read_bits(3).unwrap();
        assert_eq!(reader.byte_align().unwrap(), 5);
        assert_eq!(reader.bit_position(), 8);
        assert!(reader.is_byte_aligned());
        // 已对齐时不消耗比特
        assert_eq!(reader.byte_align().unwrap(), 0);
        assert_eq!(reader.bit_position(), 8);
    }

    // variable_bits(2)：首块 0b01，b_read_more = 0 → 值为 1
    #[test]
    fn variable_bits_single_chunk() {
        let mut reader = BitReader::new(&[0b01_0_00000]);
        assert_eq!(reader.variable_bits(2).unwrap(), 1);
        assert_eq!(reader.bit_position(), 3);
    }

    // variable_bits(2)：0b11 + more，再 0b00 + stop
    // value = 3 → <<2 = 12 → +4 = 16 → +0 = 16
    #[test]
    fn variable_bits_two_chunks() {
        let mut reader = BitReader::new(&[0b11_1_00_0_00]);
        assert_eq!(reader.variable_bits(2).unwrap(), 16);
        assert_eq!(reader.bit_position(), 6);
    }

    // 连续 b_read_more 会不断左移；溢出必须报错而不是回绕
    #[test]
    fn variable_bits_overflow_is_reported() {
        let mut reader = BitReader::new(&[0xFF; 32]);
        assert!(matches!(
            reader.variable_bits(4).unwrap_err(),
            ReadError::ValueOverflow { .. }
        ));
    }

    /// 超出 `u32` 必须报错，不能饱和或回落为小值。
    #[test]
    fn variable_bits_u32_rejects_out_of_range_value() {
        // 七组 1111+more（35 个 1）后接 0000+stop：4 581 298 432 > u32::MAX。
        let mut reader = BitReader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0b1110_0000]);
        assert!(matches!(
            reader.variable_bits_u32(4),
            Err(ReadError::ValueOverflow { bit_position: 0 })
        ));

        // 落在 u32 内的值必须原样返回。
        let mut reader = BitReader::new(&[0b0001_0000]);
        assert_eq!(reader.variable_bits_u32(4).unwrap(), 1);
    }

    /// 扩展值本身可表示时，基值相加与移位缩放仍不得回绕。
    #[test]
    fn variable_bits_scaled_u32_rejects_combination_overflow() {
        let mut reader = BitReader::new(&[0b0001_0000]);
        assert!(matches!(
            reader.variable_bits_scaled_u32(4, u32::MAX, 0),
            Err(ReadError::ValueOverflow { bit_position: 0 })
        ));

        let mut reader = BitReader::new(&[0b0001_0000]);
        assert!(matches!(
            reader.variable_bits_scaled_u32(4, 0, 32),
            Err(ReadError::ValueOverflow { bit_position: 0 })
        ));
    }

    #[test]
    fn variable_bits_truncated_input_reports_bounds() {
        // 首块读取成功后 b_read_more 缺失
        let mut reader = BitReader::new(&[0b11_u8]);
        reader.skip_bits(6).unwrap();
        assert!(matches!(
            reader.variable_bits(2).unwrap_err(),
            ReadError::OutOfBounds { .. }
        ));
    }
}
