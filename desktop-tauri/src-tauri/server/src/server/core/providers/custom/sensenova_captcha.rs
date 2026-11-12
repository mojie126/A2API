//! 商汤日日新登录页人机验证（滑块）的**程序化求解**。
//!
//! ── 为什么需要它 ──────────────────────────────────────────────
//! 发码接口（`sendSmsCode`）在风控命中时不返回 `token_code`，只回空串 —— 此时
//! 必须先完成官网滑块并带上 `code_key` 重新发码。滑块一旦出现就卡住整条登录
//! 链路，而它本身是**纯图像题**：缺口位置就在背景图上，不掺任何加密轨迹。
//!
//! ── 判据与依据 ────────────────────────────────────────────────
//! 上游一次只收一个 `code_value`（像素偏移），偏一像素即判 `invalidCaptcha`。
//! 因此解法必须精确到像素，**不能**用「差不多对齐就行」的粗匹配。
//!
//! 拼图块（`block`）是背景图（`image`）上某一段的**原样切片**（带 alpha 通道，
//! 非拼图区域透明），缺口位置就是这段切片在背景图里的原位。做法：取切片里
//! 不透明像素的**梯度**（只看左右与上下两对邻点的亮度差），把这些梯度当成
//! 模板，在背景图上逐像素平移、按内积打分，取最高分的位置。
//! 梯度只对结构敏感、对整体亮度不敏感 —— 直接比灰度会被背景的明暗纹理带偏，
//! 比梯度则能对上真正的图形轮廓。
//!
//! 2026-11-12 实测：316×190 背景 + 47×190 切片，最佳偏移 225，上游
//! `checkCaptcha?code_value=225` 返回 `{"result":true}`，而 224 / 226 均被拒。

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

/// 上游返回的图片字段常带尾部填充（实测有 128453 个 NUL），解码前先洗掉。
fn decode_png(data: &str) -> Option<image::DynamicImage> {
    let cleaned: String = data.chars().filter(|ch| !ch.is_control()).collect();
    let bytes = STANDARD.decode(cleaned.as_bytes()).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

/// 一个梯度采样点：坐标 + 该处的横纵梯度。
type Sample = (i32, i32, i32, i32);

/// 灰度图的亮度梯度（只看四邻点，不用卷积核）。
fn gradient_at(luma: &image::GrayImage, x: i32, y: i32) -> Option<(i32, i32)> {
    let width = luma.width() as i32;
    let height = luma.height() as i32;
    if x <= 0 || y <= 0 || x >= width - 1 || y >= height - 1 {
        return None;
    }
    let pixel = |px: i32, py: i32| -> i32 { luma.get_pixel(px as u32, py as u32)[0] as i32 };
    Some((pixel(x + 1, y) - pixel(x - 1, y), pixel(x, y + 1) - pixel(x, y - 1)))
}

/// 拼图切片里不透明像素上的梯度采样点。
///
/// 只取 alpha 明显大于 0 且梯度足够大的点：透明区域是填充色，算进去只会
/// 引入与背景无关的噪声；梯度太小的点（纯色内部）几乎不携带形状信息。
fn block_samples(block: &image::DynamicImage) -> Vec<Sample> {
    let rgba = block.to_rgba8();
    let luma = image::DynamicImage::ImageRgba8(rgba.clone()).to_luma8();
    let width = rgba.width() as i32;
    let height = rgba.height() as i32;
    let mut samples = Vec::new();
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            if rgba.get_pixel(x as u32, y as u32)[3] < 220 {
                continue;
            }
            let Some((gx, gy)) = gradient_at(&luma, x, y) else {
                continue;
            };
            if gx.abs() + gy.abs() <= 40 {
                continue;
            }
            samples.push((x, y, gx, gy));
        }
    }
    samples
}

/// 在背景图上定位拼图切片的原位（返回缺口左边缘的像素偏移）。
///
/// 打分用梯度的内积：背景该处也有同样走向的边缘时得分高，形状对上了才高。
pub fn solve_offset(image_b64: &str, block_b64: &str) -> Option<u32> {
    let background = decode_png(image_b64)?;
    let block = decode_png(block_b64)?;
    let samples = block_samples(&block);
    if samples.is_empty() {
        return None;
    }
    let block_width = block.width() as i32;
    let block_height = block.height() as i32;
    if (background.width() as i32) < block_width || (background.height() as i32) < block_height {
        return None;
    }
    let luma = background.to_luma8();
    let mut best_offset = 0i32;
    let mut best_score = i64::MIN;
    for offset_x in 0..=(background.width() as i32 - block_width) {
        let mut score = 0i64;
        for (x, y, gx, gy) in &samples {
            let Some((bgx, bgy)) = gradient_at(&luma, x + offset_x, *y) else {
                continue;
            };
            score += i64::from(bgx) * i64::from(*gx) + i64::from(bgy) * i64::from(*gy);
        }
        if score > best_score {
            best_score = score;
            best_offset = offset_x;
        }
    }
    Some(best_offset as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张「背景 + 切片」的题面：背景上画一批方块，切片是其中一段的
    /// 原样拷贝（透明区域填 0），缺口位置就是那一段的起点。
    ///
    /// 用合成题面而不是真实截图做样本：真实题面的 base64 有 150 KB，进不了
    /// 源码；而这段算法要验的是「切片在背景里的原位能不能被找回来」，合成
    /// 题面正好覆盖这一点，且期望值是已知的。
    ///
    /// 纹理必须是**非周期**的：早先那版按固定格子交替明暗，间隔 11 像素的
    /// 图案会在多处重复，切片挪一个周期照样对得上，求解器找回来的就是另一
    /// 个等价位置而不是缺口。真实题面是照片与插画，没有这种重复，所以样本
    /// 也要照着那个前提造。
    fn make_puzzle(gap_x: u32) -> (String, String) {
        let width = 316u32;
        let height = 190u32;
        let block_width = 47u32;
        let mut background = image::RgbImage::new(width, height);
        // 线性同余伪随机：给每个像素一个独立亮度，纹理既不重复又有清晰的边缘
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for y in 0..height {
            for x in 0..width {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let value = ((seed >> 33) as u8) % 200 + 30;
                background.put_pixel(x, y, image::Rgb([value, value / 2 + 20, 255 - value]));
            }
        }
        // 再叠几块高对比的方块，让局部轮廓更明确（纯噪点的梯度过于均匀）
        for index in 0..24u32 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let box_x = ((seed >> 33) as u32) % (width - 20);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let box_y = ((seed >> 33) as u32) % (height - 20);
            let box_w = 6 + ((seed >> 33) as u32) % 14;
            let box_h = 6 + ((seed >> 33) as u32) % 14;
            let bright = (index % 2 == 0) as u8 * 210;
            for y in box_y..(box_y + box_h).min(height) {
                for x in box_x..(box_x + box_w).min(width) {
                    background.put_pixel(x, y, image::Rgb([bright, 255 - bright, 128]));
                }
            }
        }
        let mut block = image::RgbaImage::new(block_width, height);
        for y in 0..height {
            for x in 0..block_width {
                let pixel = background.get_pixel(gap_x + x, y);
                block.put_pixel(x, y, image::Rgba([pixel[0], pixel[1], pixel[2], 255]));
            }
        }
        (
            encode_png(&image::DynamicImage::ImageRgb8(background)),
            encode_png(&image::DynamicImage::ImageRgba8(block)),
        )
    }

    fn encode_png(picture: &image::DynamicImage) -> String {
        let mut buffer = std::io::Cursor::new(Vec::new());
        picture
            .write_to(&mut buffer, image::ImageFormat::Png)
            .ok()
            .expect("写入 PNG 缓冲失败");
        STANDARD.encode(buffer.into_inner())
    }

    /// 切片在背景里的原位必须被找回来（上游只接受精确到像素的偏移）。
    #[test]
    fn solves_the_puzzle_offset() {
        for gap_x in [0u32, 37, 120, 225, 269] {
            let (background, block) = make_puzzle(gap_x);
            let solved = solve_offset(&background, &block);
            assert_eq!(Some(gap_x), solved, "缺口在 {gap_x} 时求解结果不对");
        }
    }

    /// 上游会在 base64 末尾堆一长串 NUL（实测 128453 个），解码前必须洗掉。
    #[test]
    fn tolerates_trailing_control_bytes() {
        let (background, block) = make_puzzle(225);
        let padded = format!("{background}{}", "\u{0}".repeat(64));
        assert_eq!(Some(225), solve_offset(&padded, &block));
    }

    /// 图片不是有效 PNG 时给 None，而不是panic（release 是 panic=abort）。
    #[test]
    fn invalid_image_returns_none() {
        assert_eq!(None, solve_offset("not-an-image", "also-not-an-image"));
    }
}


