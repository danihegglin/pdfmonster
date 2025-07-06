use image::{GrayImage, Luma, DynamicImage};
use ndarray::Array2;
use std::f32::consts::PI;
use imageproc::geometric_transformations::{rotate_about_center, Interpolation};
use edge_detection::{canny};

pub fn deskew() {
    let input_path = "input.png";
    let output_path = "deskewed.png";

    let img = image::open(input_path).expect("Failed to open input").to_luma8();
    let img2 = image::open(input_path).expect("Failed to open input").to_luma8();

    let detection = canny(img, 1.4, 0.8, 0.6);

    let edge_img: GrayImage = detection.as_image().to_luma8();
    edge_img.save("edges.png").unwrap();

    let acc = hough_transform(&edge_img);
    let lines = extract_lines(&acc, 100); // adjust threshold if needed

    for &(_, theta) in &lines {
        println!("theta: {:.2} degrees", theta.to_degrees());
    }

    let angles: Vec<f32> = lines
        .iter()
        .map(|&(_, theta)| theta)
        .filter(|&theta| {
            let deg = theta.to_degrees();
            deg > 20.0 && deg < 160.0 // Exclude near-horizontal/vertical
        })
        .map(|theta| theta - PI / 2.0) // convert from vertical to horizontal reference
        .collect();

    if angles.is_empty() {
        println!("No dominant skew detected.");
        return;
    }

    let avg_angle = angles.iter().copied().sum::<f32>() / angles.len() as f32;
    let angle_deg = avg_angle.to_degrees();
    println!("Detected skew angle: {:.2} degrees", angle_deg);

    let rotated = rotate_image(&DynamicImage::ImageLuma8(img2), -angle_deg);
    rotated.save(output_path).expect("Failed to save output");

    println!("Saved deskewed image to {}", output_path);
}

fn rotate_image(img: &DynamicImage, angle_deg: f32) -> DynamicImage {
    let gray = img.to_luma8();
    let radians = angle_deg.to_radians();
    let rotated = rotate_about_center(&gray, radians, Interpolation::Bilinear, Luma([255]));
    DynamicImage::ImageLuma8(rotated)
}

pub fn hough_transform(edge_img: &GrayImage) -> Array2<u32> {
    let (width, height) = edge_img.dimensions();
    let diag_len = ((width.pow(2) + height.pow(2)) as f32).sqrt().ceil() as usize;
    let rho_max = diag_len * 2;
    let theta_bins = 180;

    let mut accumulator = Array2::<u32>::zeros((rho_max, theta_bins));

    for y in 0..height {
        for x in 0..width {
            let pixel = edge_img.get_pixel(x, y).0[0];
            if pixel < 100 {
                continue;
            }

            for theta_idx in 0..theta_bins {
                let theta = (theta_idx as f32) * PI / 180.0;
                let rho = (x as f32) * theta.cos() + (y as f32) * theta.sin();
                let rho_idx = ((rho + diag_len as f32).round()) as usize;

                if rho_idx < rho_max {
                    accumulator[(rho_idx, theta_idx)] += 1;
                }
            }
        }
    }

    accumulator
}

pub fn extract_lines(acc: &Array2<u32>, threshold: u32) -> Vec<(f32, f32)> {
    let mut lines = Vec::new();
    let diag_len = acc.nrows() / 2;

    for (rho_idx, row) in acc.outer_iter().enumerate() {
        for (theta_idx, &val) in row.iter().enumerate() {
            if val > threshold {
                let rho = rho_idx as f32 - diag_len as f32;
                let theta = theta_idx as f32 * PI / 180.0;
                lines.push((rho, theta));
            }
        }
    }

    lines
}
