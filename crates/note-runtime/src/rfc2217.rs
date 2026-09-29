//! RFC 2217 (telnet COM-PORT-OPTION) server side for one serial port (DEV-02, ADR-011).
//! esptool/pyserial open `rfc2217://host:port`, negotiate telnet options, set line parameters
//! (acknowledged but not simulated: the emulated UART has no baud-rate timing) and drive DTR/RTS,
//! which the runtime maps to the classic auto-reset circuit.

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const BINARY: u8 = 0;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const COM_PORT_OPTION: u8 = 44;

const SET_CONTROL: u8 = 5;
const SERVER_OFFSET: u8 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lines {
    pub dtr: bool,
    pub rts: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Serial bytes from the client, unescaped.
    Data(Vec<u8>),
    /// DTR or RTS changed; the new state of both.
    Control(Lines),
}

#[derive(Default)]
pub struct Rfc2217 {
    inbuf: Vec<u8>,
    /// Bytes to send back to the client (negotiation replies, acknowledgements).
    pub reply: Vec<u8>,
    pub lines: Lines,
}

impl Rfc2217 {
    /// Telnet options offered when the client connects (pyserial waits for these).
    pub fn greeting() -> Vec<u8> {
        vec![IAC, WILL, SGA, IAC, WILL, ECHO, IAC, DO, COM_PORT_OPTION, IAC, WILL, BINARY, IAC, DO, BINARY]
    }

    /// Escape serial data for the client.
    pub fn encode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            out.push(b);
            if b == IAC {
                out.push(IAC);
            }
        }
        out
    }

    /// Feed bytes from the client; returns what they mean, in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.inbuf.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut data = Vec::new();
        let mut i = 0;
        while i < self.inbuf.len() {
            let b = self.inbuf[i];
            if b != IAC {
                data.push(b);
                i += 1;
                continue;
            }
            let Some(&cmd) = self.inbuf.get(i + 1) else { break };
            match cmd {
                IAC => {
                    data.push(IAC);
                    i += 2;
                }
                WILL | WONT | DO | DONT => {
                    let Some(&opt) = self.inbuf.get(i + 2) else { break };
                    self.negotiate(cmd, opt);
                    i += 3;
                }
                SB => {
                    // IAC SB ... IAC SE, with IAC IAC inside the parameters.
                    let mut j = i + 2;
                    let mut body = Vec::new();
                    let mut complete = false;
                    while j < self.inbuf.len() {
                        if self.inbuf[j] == IAC {
                            match self.inbuf.get(j + 1) {
                                Some(&SE) => { complete = true; j += 2; break; }
                                Some(&IAC) => { body.push(IAC); j += 2; }
                                Some(_) => { j += 2; }
                                None => break,
                            }
                        } else {
                            body.push(self.inbuf[j]);
                            j += 1;
                        }
                    }
                    if !complete {
                        break;
                    }
                    if !data.is_empty() {
                        events.push(Event::Data(std::mem::take(&mut data)));
                    }
                    if let Some(ev) = self.subnegotiation(&body) {
                        events.push(ev);
                    }
                    i = j;
                }
                _ => i += 2, // NOP, AYT, ...: ignored
            }
        }
        self.inbuf.drain(..i);
        if !data.is_empty() {
            events.push(Event::Data(data));
        }
        events
    }

    fn negotiate(&mut self, cmd: u8, opt: u8) {
        let supported = matches!(opt, BINARY | ECHO | SGA | COM_PORT_OPTION);
        let answer = match (cmd, supported) {
            (WILL, true) => DO,
            (WILL, false) => DONT,
            (DO, true) => WILL,
            (DO, false) => WONT,
            _ => return, // WONT/DONT need no reply
        };
        self.reply.extend_from_slice(&[IAC, answer, opt]);
    }

    fn subnegotiation(&mut self, body: &[u8]) -> Option<Event> {
        let (&option, rest) = body.split_first()?;
        if option != COM_PORT_OPTION {
            return None;
        }
        let (&cmd, value) = rest.split_first()?;
        let mut ack = value.to_vec();
        let mut event = None;
        if cmd == SET_CONTROL {
            let before = self.lines;
            match value.first() {
                Some(8) => self.lines.dtr = true,
                Some(9) => self.lines.dtr = false,
                Some(11) => self.lines.rts = true,
                Some(12) => self.lines.rts = false,
                Some(7) => ack = vec![if self.lines.dtr { 8 } else { 9 }],   // request DTR state
                Some(10) => ack = vec![if self.lines.rts { 11 } else { 12 }], // request RTS state
                Some(0) => ack = vec![1],                                      // flow control: none
                _ => {}
            }
            if self.lines != before {
                event = Some(Event::Control(self.lines));
            }
        }
        // Every client request is acknowledged with the server code (cmd + 100) and its value.
        self.reply.extend_from_slice(&[IAC, SB, COM_PORT_OPTION, cmd + SERVER_OFFSET]);
        self.reply.extend(Self::encode(&ack));
        self.reply.extend_from_slice(&[IAC, SE]);
        event
    }
}

/// The classic esptool auto-reset circuit: RTS holds EN (reset) low, DTR holds GPIO0 low.
pub fn en_and_gpio0(lines: Lines) -> (bool, bool) {
    (!lines.rts, !lines.dtr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_accepts_rfc2217_and_refuses_others() {
        let mut s = Rfc2217::default();
        assert!(s.feed(&[IAC, WILL, COM_PORT_OPTION, IAC, DO, SGA, IAC, WILL, 24]).is_empty());
        assert_eq!(s.reply, [IAC, DO, COM_PORT_OPTION, IAC, WILL, SGA, IAC, DONT, 24]);
    }

    #[test]
    fn data_is_unescaped_and_control_is_acknowledged() {
        let mut s = Rfc2217::default();
        let ev = s.feed(&[0xc0, IAC, IAC, 0x01, IAC, SB, COM_PORT_OPTION, SET_CONTROL, 11, IAC, SE, 0x02]);
        assert_eq!(ev, [Event::Data(vec![0xc0, 0xff, 0x01]), Event::Control(Lines { dtr: false, rts: true }), Event::Data(vec![0x02])]);
        assert_eq!(s.reply, [IAC, SB, COM_PORT_OPTION, 105, 11, IAC, SE]);
    }

    #[test]
    fn split_packets_are_reassembled() {
        let mut s = Rfc2217::default();
        let mut events = Vec::new();
        for b in [IAC, SB, COM_PORT_OPTION, 1, 0, 1, 0xc2, 0, IAC, SE] {
            events.extend(s.feed(&[b]));
        }
        assert!(events.is_empty(), "baud rate is acknowledged, not an event");
        assert_eq!(s.reply, [IAC, SB, COM_PORT_OPTION, 101, 0, 1, 0xc2, 0, IAC, SE]);
    }

    #[test]
    fn esptool_classic_reset_releases_en_with_gpio0_low() {
        // DTR=0 RTS=1 (reset held) -> DTR=1 (still held) -> RTS=0 (released, GPIO0 low).
        assert_eq!(en_and_gpio0(Lines { dtr: false, rts: true }), (false, true));
        assert_eq!(en_and_gpio0(Lines { dtr: true, rts: true }), (false, false));
        assert_eq!(en_and_gpio0(Lines { dtr: true, rts: false }), (true, false));
        assert_eq!(Rfc2217::encode(&[1, 0xff, 2]), [1, 0xff, 0xff, 2]);
    }
}
