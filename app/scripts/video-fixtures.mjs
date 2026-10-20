// Lossless stored YUV values and authored timestamps, independent of FFmpeg's
// RGB conversion. Used by native decoder tests and Windows playback smoke.
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
export const directory = path.join(root, "target/video-fixtures");
export const ffmpeg = process.env.RINKA_FFMPEG_BIN ?? (process.platform === "win32"
  ? path.join(root, "target/windows-tools/ffmpeg-9.0.2-full_build-shared/bin/ffmpeg.exe") : "ffmpeg");

export function generateFixtures() {
  mkdirSync(directory, { recursive: true });
  const encode = (name, source, args = []) => execFileSync(ffmpeg, [
    "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", source,
    "-t", "1.2", "-an", ...args, path.join(directory, name),
  ], { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  const constant = (size) => `nullsrc=s=${size}:r=25,geq=lum=97:cb=138:cr=90`;
  // Set frame metadata before encoding too: recent FFmpeg versions otherwise
  // insert a color conversion to satisfy the requested output tags.
  encode("bt709-limited.mkv", constant("64x64") + ",setparams=colorspace=bt709:range=limited", ["-c:v", "ffv1", "-pix_fmt", "yuv420p", "-colorspace", "bt709", "-color_range", "tv"]);
  encode("bt601-full.mkv", constant("64x64") + ",setparams=colorspace=smpte170m:range=full", ["-c:v", "ffv1", "-pix_fmt", "yuv420p", "-colorspace", "smpte170m", "-color_range", "pc"]);
  encode("bt709-444.mkv", `nullsrc=s=64x64:r=25,format=yuv444p,geq=lum=97:cb=138:cr=90,setparams=colorspace=bt709:range=limited`, ["-c:v", "ffv1", "-pix_fmt", "yuv444p", "-colorspace", "bt709", "-color_range", "tv"]);
  encode("bt601-full-444.avi", `nullsrc=s=64x64:r=25,format=yuv444p,geq=lum=97:cb=138:cr=90,setparams=colorspace=smpte170m:range=full`, ["-c:v", "mjpeg", "-q:v", "1", "-pix_fmt", "yuvj444p", "-colorspace", "smpte170m", "-color_range", "pc"]);
  encode("untagged-sd.mkv", constant("64x64"), ["-c:v", "ffv1", "-pix_fmt", "yuv420p"]);
  encode("untagged-hd.mkv", constant("64x720"), ["-c:v", "ffv1", "-pix_fmt", "yuv420p"]);
  encode("vfr.mkv", constant("64x64"), ["-vf", "select='eq(mod(n,5),0)+eq(mod(n,5),1)'", "-fps_mode", "vfr", "-c:v", "ffv1", "-pix_fmt", "yuv420p"]);
  encode("bframes.mp4", "testsrc2=s=320x180:r=30", ["-c:v", "libx264", "-pix_fmt", "yuv420p", "-bf", "3", "-g", "30", "-colorspace", "bt709", "-color_range", "tv"]);
  encode("preview-full.mp4", constant("64x64") + ",setparams=colorspace=smpte170m:range=full", ["-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p", "-colorspace", "smpte170m", "-color_range", "pc"]);
  console.log(`Video fixtures: ${directory}`);
  return directory;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) generateFixtures();
