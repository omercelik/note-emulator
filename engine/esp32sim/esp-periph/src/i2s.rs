//! I2S TX and RX. TX is the clock tree and the sample sink the SoC's DMA pump fills.
//! RX is the microphone: a WAV (or the speaker loopback) queued by the runtime, then
//! written into the guest's GDMA IN buffers at the programmed sample rate.
use std::collections::VecDeque;

use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;

/// Bytes held for the microphone before the guest has a descriptor ready.
const RX_PENDING_MAX: usize = 64 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct I2s {
    pub rx_conf: u32, pub tx_conf: u32, pub int_raw: u32, pub int_ena: u32,
    ram: RegRam,
    /// TX_CONF1 (0x2c), TX_CLKM_CONF (0x34), TX_CLKM_DIV_CONF (0x3c), TX_TDM_CTRL (0x54).
    pub tx_conf1: u32, pub tx_clkm_conf: u32, pub tx_clkm_div_conf: u32, pub tx_tdm_ctrl: u32,
    /// RX_CONF1 (0x28), RX_CLKM_CONF (0x30), RX_CLKM_DIV_CONF (0x38), RX_TDM_CTRL (0x50), RXEOF_NUM (0x64).
    pub rx_conf1: u32, pub rx_clkm_conf: u32, pub rx_clkm_div_conf: u32, pub rx_tdm_ctrl: u32,
    pub rx_eof_num: u32,
    /// Frame rate on the wire. 44.1 kHz until firmware programs the clock or a WAV names its rate.
    pub sample_rate: u32,
    pub rx_sample_rate: u32,
    pub bytes_per_frame: u32,
    pub rx_bytes_per_frame: u32,
    acc: u64,
    rx_acc: u64,
    /// Bytes of the current RX_EOF_NUM window already handed to GDMA.
    pub rx_eof_bytes: u32,
    /// Decoded left-channel samples (host sink).
    pub pcm: Vec<i16>,
    pub frames_out: u64,
    pub frames_in: u64,
    /// Little-endian PCM bytes waiting to be DMA'd into the guest (microphone and loopback).
    pub rx_pending: VecDeque<u8>,
    /// Copy each TX DMA frame onto the RX queue while the receiver is running.
    pub loopback: bool,
    /// A quiet mic (no WAV, no loopback) still completes DMA with zeros so a read does not hang.
    /// Tests that check exact WAV bytes turn this off.
    pub rx_idle_silence: bool,
    pub tx_started_log: bool,
    cpu_hz: u64,
}

impl I2s {
    pub fn new(cpu_hz: u64) -> Self {
        I2s {
            cpu_hz, rx_conf: 0, tx_conf: 0, int_raw: 0, int_ena: 0, ram: RegRam::new(),
            tx_conf1: 0, tx_clkm_conf: 0, tx_clkm_div_conf: 0, tx_tdm_ctrl: 0xffff,
            rx_conf1: 0, rx_clkm_conf: 0, rx_clkm_div_conf: 0, rx_tdm_ctrl: 0xffff, rx_eof_num: 0,
            sample_rate: 44100, rx_sample_rate: 44100, bytes_per_frame: 1, rx_bytes_per_frame: 1,
            acc: 0, rx_acc: 0, rx_eof_bytes: 0, pcm: Vec::new(), frames_out: 0, frames_in: 0,
            rx_pending: VecDeque::new(), loopback: true, rx_idle_silence: true, tx_started_log: false,
        }
    }
    pub fn tx_running(&self) -> bool { self.tx_conf & (1 << 2) != 0 }
    pub fn rx_running(&self) -> bool { self.rx_conf & (1 << 2) != 0 }
    /// Packed DMA sample width, independent of padding in the wire's time slots.
    pub fn sample_bytes(&self) -> usize { (((self.tx_conf1 >> 13) & 0x1f) + 1).div_ceil(8) as usize }
    pub fn rx_sample_bytes(&self) -> usize { (((self.rx_conf1 >> 13) & 0x1f) + 1).div_ceil(8) as usize }
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0xc => self.int_raw, 0x10 => self.int_raw & self.int_ena, 0x14 => self.int_ena,
            0x20 => self.rx_conf & !(1 << 8) & !3, 0x24 => self.tx_conf & !(1 << 8) & !3,   // update/reset bits self-clear
            0x28 => self.rx_conf1, 0x30 => self.rx_clkm_conf, 0x38 => self.rx_clkm_div_conf,
            0x50 => self.rx_tdm_ctrl, 0x64 => self.rx_eof_num,
            0x6c => if self.tx_running() { 0 } else { 1 },                                   // STATE: tx_idle
            0x54 => self.tx_tdm_ctrl,
            0x80 => 0x2003070,
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x14 => self.int_ena = v, 0x18 => self.int_raw &= !v,
            0x20 => {
                if v & 1 != 0 { self.rx_acc = 0; self.rx_eof_bytes = 0; }
                self.rx_conf = v;
                self.update_rx();
            }
            0x24 => { self.tx_conf = v; self.update_rate(); }
            0x28 => { self.rx_conf1 = v; self.ram.write(off, v); self.update_rx(); }
            0x2c => { self.tx_conf1 = v; self.ram.write(off, v); self.update_rate(); }
            0x30 => { self.rx_clkm_conf = v; self.ram.write(off, v); self.update_rx(); }
            0x34 => { self.tx_clkm_conf = v; self.ram.write(off, v); self.update_rate(); }
            0x38 => { self.rx_clkm_div_conf = v; self.ram.write(off, v); self.update_rx(); }
            0x3c => { self.tx_clkm_div_conf = v; self.ram.write(off, v); self.update_rate(); }
            0x50 => { self.rx_tdm_ctrl = v; self.ram.write(off, v); self.update_rx(); }
            0x54 => { self.tx_tdm_ctrl = v; self.ram.write(off, v); self.update_rate(); }
            0x64 => self.rx_eof_num = v & 0xfff,
            _ => self.ram.write(off, v),
        }
    }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
    /// The frame rate the clock tree produces. `clkm` is CLK_SEL/CLK_ACTIVE/div_num,
    /// `div` is the fractional divider, `conf1` is BCK and half-sample width.
    ///   MCLK = src / (div_num + b/a)   with b/a recovered from x/y/z/yn1 the way
    ///                                  `i2s_ll_*_set_mclk` encodes them
    ///   BCK  = MCLK / (bck_div_num + 1)
    ///   fs   = BCK / (2 · (half_sample_bits + 1))
    /// CLK_SEL matches the IDF programming (0 XTAL, 1 PLL240, 2 PLL160), not the
    /// register-header comment. Returns None while the clock is off or unprogrammed.
    pub fn derive_fs(clkm: u32, div: u32, conf1: u32) -> Option<u32> {
        if clkm & (1 << 26) == 0 { return None; }
        let src: u64 = match (clkm >> 27) & 3 { 0 => 40_000_000, 1 => 240_000_000, 2 => 160_000_000, _ => return None };
        let n = (clkm & 0xff) as u64;
        if n == 0 { return None; }
        let (z, y, x, yn1) = ((div & 0x1ff) as u64, ((div >> 9) & 0x1ff) as u64, ((div >> 18) & 0x1ff) as u64, div & (1 << 27) != 0);
        let (a, b) = if z == 0 { (1, 0) } else { let a = (x + 1) * z + y; (a, if yn1 { a - z } else { z }) };
        let bck = ((conf1 >> 7) & 0x3f) as u64 + 1;
        let frame_bits = 2 * (((conf1 >> 18) & 0x3f) as u64 + 1);
        let denom = (n * a + b) * bck * frame_bits;
        if denom == 0 { return None; }
        let fs = (src * a + denom / 2) / denom;
        if !(1_000..=400_000).contains(&fs) { return None; }
        Some(fs as u32)
    }
    pub fn derive_rate(&self) -> Option<u32> { Self::derive_fs(self.tx_clkm_conf, self.tx_clkm_div_conf, self.tx_conf1) }
    pub fn derive_rx_rate(&self) -> Option<u32> { Self::derive_fs(self.rx_clkm_conf, self.rx_clkm_div_conf, self.rx_conf1) }
    fn dma_bytes(conf: u32, tdm: u32, sample_bytes: u32) -> u32 {
        let slots = ((tdm >> 16) & 0xf) + 1;
        let active = (tdm & ((1 << slots) - 1)).count_ones();
        // Mono (bit 5) puts one slot in the DMA buffer. SKIP (TDM bit 20) consumes
        // inactive slots too. Otherwise only enabled slots have data.
        let dma_slots = if conf & (1 << 5) != 0 { u32::from(active != 0) }
            else if tdm & (1 << 20) != 0 { slots } else { active };
        sample_bytes * dma_slots
    }
    fn update_rate(&mut self) {
        if let Some(fs) = self.derive_rate() { self.sample_rate = fs; }
        self.bytes_per_frame = Self::dma_bytes(self.tx_conf, self.tx_tdm_ctrl, self.sample_bytes() as u32);
    }
    fn update_rx(&mut self) {
        if let Some(fs) = self.derive_rx_rate() { self.rx_sample_rate = fs; }
        self.rx_bytes_per_frame = Self::dma_bytes(self.rx_conf, self.rx_tdm_ctrl, self.rx_sample_bytes() as u32);
    }
    /// Number of TX frames due after `cycles` CPU cycles at the configured sample rate.
    pub fn frames_due(&mut self, cycles: u64) -> u32 {
        if !self.tx_running() { self.acc = 0; return 0; }
        self.acc += cycles * self.sample_rate as u64;
        let n = (self.acc / self.cpu_hz) as u32;
        self.acc %= self.cpu_hz;
        n
    }
    /// Number of RX frames due. Independent of the TX accumulator.
    pub fn frames_due_rx(&mut self, cycles: u64) -> u32 {
        if !self.rx_running() { self.rx_acc = 0; return 0; }
        self.rx_acc += cycles * self.rx_sample_rate.max(1) as u64;
        let n = (self.rx_acc / self.cpu_hz) as u32;
        self.rx_acc %= self.cpu_hz;
        n
    }
    /// Stop mode 1: RX_START clears when `in_suc_eof` fires.
    pub fn rx_stop_on_eof(&self) -> bool { (self.rx_conf >> 13) & 3 == 1 }
    pub fn clear_rx_start(&mut self) { self.rx_conf &= !(1 << 2); }

    pub fn push_rx_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() { return; }
        if bytes.len() >= RX_PENDING_MAX {
            self.rx_pending.clear();
            self.rx_pending.extend(bytes[bytes.len() - RX_PENDING_MAX..].iter().copied());
            return;
        }
        let overflow = self.rx_pending.len().saturating_add(bytes.len()).saturating_sub(RX_PENDING_MAX);
        if overflow > 0 {
            let n = overflow.min(self.rx_pending.len());
            self.rx_pending.drain(..n);
        }
        let room = RX_PENDING_MAX.saturating_sub(self.rx_pending.len());
        self.rx_pending.extend(bytes.iter().take(room).copied());
    }

    /// Queue a PCM WAV as the microphone. Samples are interleaved little-endian i16,
    /// one per channel, which is what a 16-bit DMA descriptor reads back.
    pub fn push_rx_wav(&mut self, wav: &[u8]) -> Result<usize, &'static str> {
        let parsed = parse_wav_pcm(wav)?;
        let mut bytes = Vec::with_capacity(parsed.samples.len() * 2);
        for s in &parsed.samples { bytes.extend_from_slice(&s.to_le_bytes()); }
        let n = parsed.samples.len();
        self.push_rx_bytes(&bytes);
        if self.derive_rx_rate().is_none() { self.rx_sample_rate = parsed.rate; }
        Ok(n)
    }

    /// 16-bit mono WAV of the captured speaker samples.
    pub fn wav_from_pcm(samples: &[i16], rate: u32) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples { out.extend_from_slice(&s.to_le_bytes()); }
        out
    }
}

/// PCM WAV the microphone path accepts: format 1, 8- or 16-bit, 1 or 2 channels.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WavPcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

pub fn parse_wav_pcm(bytes: &[u8]) -> Result<WavPcm, &'static str> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" { return Err("not a WAV"); }
    let mut fmt = None;
    let mut data = None;
    let mut off = 12usize;
    while off + 8 <= bytes.len() {
        let id = &bytes[off..off + 4];
        let size = u32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap()) as usize;
        let body = off + 8;
        let end = body.checked_add(size).ok_or("WAV chunk size overflows")?;
        if end > bytes.len() { return Err("WAV chunk is truncated"); }
        if id == b"fmt " && end - body >= 16 {
            fmt = Some(&bytes[body..end]);
        } else if id == b"data" {
            data = Some(&bytes[body..end]);
        }
        off = end.checked_add(size & 1).ok_or("WAV chunk size overflows")?;
    }
    let fmt = fmt.ok_or("WAV has no fmt chunk")?;
    let data = data.ok_or("WAV has no data chunk")?;
    let format = u16::from_le_bytes(fmt[0..2].try_into().unwrap());
    let channels = u16::from_le_bytes(fmt[2..4].try_into().unwrap());
    let rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes(fmt[14..16].try_into().unwrap());
    if format != 1 { return Err("WAV is not PCM"); }
    if !(1..=2).contains(&channels) { return Err("WAV channel count is not 1 or 2"); }
    if bits != 8 && bits != 16 { return Err("WAV is not 8- or 16-bit"); }
    if rate == 0 { return Err("WAV sample rate is zero"); }
    let width = (bits / 8) as usize;
    let frame = width * channels as usize;
    if frame == 0 || data.len() % frame != 0 { return Err("WAV data is not a whole number of frames"); }
    let mut samples = Vec::with_capacity(data.len() / width);
    for frame_bytes in data.chunks(frame) {
        for ch in 0..channels as usize {
            let s = &frame_bytes[ch * width..(ch + 1) * width];
            let sample = if bits == 8 { ((s[0] as i16) - 128) << 8 } else { i16::from_le_bytes([s[0], s[1]]) };
            samples.push(sample);
        }
    }
    Ok(WavPcm { rate, channels, samples })
}

impl Device for I2s {
    fn read(&mut self, off: u32) -> u32 { I2s::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { I2s::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_pcm_round_trips_known_samples() {
        let samples = [1000, -2000, 3000, -4000];
        let wav = I2s::wav_from_pcm(&samples, 16_000);
        let parsed = parse_wav_pcm(&wav).unwrap();
        assert_eq!(parsed.rate, 16_000);
        assert_eq!(parsed.channels, 1);
        assert_eq!(parsed.samples, samples);
    }

    #[test]
    fn push_rx_wav_queues_the_data_chunk_and_adopts_its_rate() {
        let samples = [1, -2, 3];
        let wav = I2s::wav_from_pcm(&samples, 16_000);
        let mut i2s = I2s::new(240_000_000);
        assert_eq!(i2s.push_rx_wav(&wav).unwrap(), 3);
        assert_eq!(i2s.rx_sample_rate, 16_000);
        let queued: Vec<u8> = i2s.rx_pending.iter().copied().collect();
        let mut expect = Vec::new();
        for s in samples { expect.extend_from_slice(&s.to_le_bytes()); }
        assert_eq!(queued, expect);
    }

    #[test]
    fn malformed_wav_chunk_is_rejected_and_rx_queue_retains_newest_audio() {
        let mut wav = I2s::wav_from_pcm(&[1, 2], 16_000);
        wav[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_wav_pcm(&wav).is_err());

        let mut i2s = I2s::new(240_000_000);
        let input: Vec<u8> = (0..RX_PENDING_MAX + 32).map(|n| n as u8).collect();
        i2s.push_rx_bytes(&input);
        assert_eq!(i2s.rx_pending.len(), RX_PENDING_MAX);
        assert_eq!(i2s.rx_pending.front(), input.get(32));
        assert_eq!(i2s.rx_pending.back(), input.last());
    }
}
