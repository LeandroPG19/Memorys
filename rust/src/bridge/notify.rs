//! What the bridge tells the person at the desk when its link to the host changes, and how.
//! A notice never blocks the bridge: on Windows it starts PowerShell and does not wait for it,
//! anywhere else it is one log line.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Serialize;

/// The script that shows a notice on Windows. Embedded, so the notice needs no file on disk
/// and no module.
pub const AVISO_PS1: &str = include_str!("aviso.ps1");

pub const POWERSHELL: &str = "powershell.exe";

/// A change the person should hear about once, not a state polled in a loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notice {
    /// The first success of this run.
    Connected {
        host_name: String,
        address: SocketAddr,
    },
    /// No success for at least 90 s. `report` is what was tried, as an operator reads it.
    Lost {
        host_name: String,
        last_good: Option<SocketAddr>,
        fingerprint: String,
        down_for_secs: u64,
        report: String,
        exe: PathBuf,
    },
    /// A success after a `Lost`.
    Reconnected {
        host_name: String,
        address: SocketAddr,
    },
    /// The credential was revoked or is unknown, or the host changed its fingerprint: only a
    /// new pairing fixes it.
    NeedsPair {
        host_name: String,
        fingerprint: String,
        reason: String,
        exe: PathBuf,
    },
}

/// Shows a notice. Must return at once: the bridge calls it from its watcher.
pub trait Notifier: Send + Sync {
    fn notify(&self, notice: Notice);
}

/// PowerShell on Windows, a log line elsewhere.
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn notify(&self, _notice: Notice) {
        unimplemented!("pasada verde: B2")
    }
}

/// The arguments `powershell.exe` is started with for `notice`.
pub fn powershell_argv(_notice: &Notice) -> Vec<String> {
    unimplemented!("pasada verde: B2")
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\ana\AppData\Local\MemoryIndustry\bin\memory-industry.exe";

    /// Windows' CreateProcess refuses a command line of 32 767 characters or more.
    const WINDOWS_COMMAND_LINE: usize = 32_767;

    fn base64_decode(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        assert_eq!(
            text.len() % 4,
            0,
            "Convert.FromBase64String refuses base64 without its padding: {text:.60}"
        );
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in text.bytes().filter(|c| *c != b'=') {
            let value = ALPHABET
                .iter()
                .position(|a| *a == c)
                .unwrap_or_else(|| panic!("{:?} is not a base64 character", char::from(c)));
            acc = (acc << 6) | u32::try_from(value).expect("a 6-bit value");
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(u8::try_from((acc >> bits) & 0xff).expect("one byte"));
            }
        }
        out
    }

    fn utf16le(bytes: &[u8]) -> String {
        assert_eq!(bytes.len() % 2, 0, "UTF-16LE comes in pairs of bytes");
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).expect("the encoded command is valid UTF-16")
    }

    /// The text PowerShell runs: what `-EncodedCommand` carries, decoded.
    fn command_text(argv: &[String]) -> String {
        let encoded = argv
            .last()
            .expect("the encoded command is the last argument");
        utf16le(&base64_decode(encoded))
    }

    /// The notice as the script receives it: the base64 of its UTF-8 JSON, in the first
    /// `FromBase64String('…')` of the command. Base64 has no quote, no `$` and no backtick,
    /// so no host name can end the literal and run as code.
    fn payload(text: &str) -> serde_json::Value {
        let open = "FromBase64String('";
        let start = text
            .find(open)
            .unwrap_or_else(|| panic!("the command carries no {open}…'): {text:.300}"))
            + open.len();
        let len = text[start..]
            .find("')")
            .expect("the base64 literal is closed");
        let json = String::from_utf8(base64_decode(&text[start..start + len]))
            .expect("the payload is UTF-8");
        serde_json::from_str(&json).expect("the payload is JSON")
    }

    fn command_line_len(argv: &[String]) -> usize {
        POWERSHELL.len() + argv.iter().map(|arg| 1 + arg.len()).sum::<usize>()
    }

    fn lost(host_name: &str, report: String) -> Notice {
        Notice::Lost {
            host_name: host_name.to_string(),
            last_good: Some("192.168.1.20:8787".parse().expect("an address")),
            fingerprint: "AB12-CD34-EF56-7890".to_string(),
            down_for_secs: 95,
            report,
            exe: PathBuf::from(EXE),
        }
    }

    /// B2: `-EncodedCommand` is how the notice gets past the execution policy, which does not
    /// apply to it, and `-WindowStyle Hidden` keeps a console from flashing on the desk.
    #[test]
    fn el_aviso_va_como_un_comando_codificado_que_lleva_el_guion_y_los_datos() {
        let notice = lost(
            "madre",
            "192.168.1.20:8787 (última buena): sin respuesta".into(),
        );

        let argv = powershell_argv(&notice);

        assert_eq!(
            argv[..argv.len() - 1],
            [
                "-NoProfile",
                "-WindowStyle",
                "Hidden",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand"
            ],
            "the flags of B2, in that order, and the encoded command last"
        );
        let text = command_text(&argv);
        assert!(
            text.contains(AVISO_PS1),
            "the command runs the embedded aviso.ps1 as it is, not a copy that can drift"
        );
        assert_eq!(
            payload(&text),
            serde_json::to_value(&notice).expect("a notice is JSON"),
            "the script receives the whole notice, field by field"
        );
    }

    /// B2: the host's name comes from the host. A quote (also the typographic ones PowerShell
    /// accepts as quotes), a `$(…)` or a backtick in it must stay data.
    #[test]
    fn un_nombre_de_madre_con_comillas_no_sale_del_literal_y_no_se_ejecuta() {
        let evil = "ma'dre’‘$(Remove-Item C:\\pwned-7f3a)`\n\"x\"";
        let notice = Notice::Connected {
            host_name: evil.to_string(),
            address: "192.168.1.20:8787".parse().expect("an address"),
        };

        let text = command_text(&powershell_argv(&notice));

        assert!(
            !text.contains("pwned-7f3a"),
            "the name reached the command as text PowerShell parses: {text:.400}"
        );
        assert_eq!(
            payload(&text)["host_name"],
            evil,
            "and the script still gets the name exactly as the host gave it"
        );
    }

    /// B2: `Lost` carries what was tried. A sweep of a whole /24 is 254 lines, and an encoded
    /// command grows to about 2.7 characters per byte of script: unbounded, the notice that
    /// matters most is the one Windows refuses to start.
    #[test]
    fn un_informe_de_un_barrido_entero_cabe_en_la_linea_de_comandos_de_windows() {
        let report = (1..=254)
            .map(|last| {
                format!(
                    "192.168.1.{last}:8787 (barrido): sin respuesta (conexión rechazada o sin ruta)"
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let notice = lost("madre", report);

        let argv = powershell_argv(&notice);

        let length = command_line_len(&argv);
        assert!(
            length < WINDOWS_COMMAND_LINE,
            "the command line is {length} characters; Windows refuses {WINDOWS_COMMAND_LINE} or more"
        );
        let shown = payload(&command_text(&argv));
        assert_eq!(shown["kind"], "lost");
        assert_eq!(
            shown["host_name"], "madre",
            "whatever is cut, the name stays"
        );
        assert_eq!(
            shown["last_good"], "192.168.1.20:8787",
            "and so does the last good address"
        );
        assert_eq!(shown["exe"], EXE, "and the path the buttons run");
    }
}
