# SideWire

[English](README.md)

SideWire는 루팅된 Android 기기를 Windows, macOS, Linux에서 제어하기 위한 네이티브 브리지입니다. ADB에 의존하지 않습니다.

KernelSU 모듈로 설치되는 Rust 데몬과, PC 쪽은 Rust로 작성된 CLI/server를 제공합니다.

기본 설정은 보안 연결이며, 페어링 이후의 일반 통신은 인증과 암호화가 적용됩니다.

ADB 의 대부분의 기능을 지원하지만, 내부적으로 ADB 를 사용하지 않기 때문에 Detect 되지 않습니다.
## 주요 기능

- 대화형 shell / PTY와 단일 명령 실행
- root 또는 shell 권한으로 실행
- 디렉터리 포함 파일 push / pull
- PC ↔ Android 텍스트 클립보드
- TCP forward / reverse tunnel
- inbound / outbound 연결 모드
- 여러 기기 동시 관리
- 일회용 PIN 페어링과 이후 자동 암호화 연결

## 요구 사항

- arm64 Android 기기
- KernelSU 모듈을 사용할 수 있는 환경
- Windows x86_64, macOS(Apple Silicon/Intel), Linux x86_64
- PC와 Android가 같은 LAN/VPN에서 서로 통신 가능해야 함

릴리스 바이너리와 KernelSU ZIP은 GitHub Releases에서 배포합니다.

## 빠른 시작

1. SideWire KernelSU 모듈을 설치하고 WebUI를 엽니다.
2. `Secure`를 선택하고 연결 모드/주소를 설정한 뒤 SideWire를 시작합니다.
3. **Generate pairing PIN**을 누릅니다.
4. PC에서 한 번만 페어링합니다.

```powershell
sidewire pair 192.168.0.123
```

페어링할 때 PC의 `sidewire server`가 켜져 있을 필요는 없습니다. WebUI에 표시된 6자리 PIN만 입력하면 됩니다.

기본 outbound 모드라면 페어링 후 PC에서 server를 실행합니다.

```powershell
sidewire server
```

다른 터미널에서 기기를 확인하고 바로 사용할 수 있습니다.

```powershell
sidewire devices
sidewire shell
sidewire exec --as root id
```

## 클립보드와 파일 전송

```powershell
sidewire clipboard push
sidewire clipboard pull
sidewire push .\build /data/local/tmp/build
sidewire pull /sdcard/MyFolder .\MyFolder
```

`clipboard push`는 PC 클립보드를 Android로 보내고, `pull`은 Android 클립보드를 PC로 가져옵니다.

## 연결 모드

**Outbound**는 Android가 PC의 `sidewire server`로 접속하는 기본 방식입니다.

**Inbound**에서는 Android가 연결을 기다리고 PC가 직접 접속합니다.

```powershell
sidewire server --connect 192.168.0.123:58321
```

Inbound 기기는 `sidewire discover` 또는 `sidewire server --discover`로 찾을 수도 있습니다.

## 보안

Secure 모드는 짧게 유지되는 페어링 PIN에 SPAKE2를 사용하고, 이후 세션은 Noise로 인증 및 암호화합니다. PIN 자체를 연결 비밀번호로 계속 사용하는 구조는 아닙니다.

신뢰할 수 있는 개발용 네트워크를 위한 plaintext 모드도 있지만 양쪽에서 직접 켜야 하며, secure 연결이 자동으로 plaintext로 내려가지는 않습니다.

보안 취약점 제보 방법은 [SECURITY.md](SECURITY.md)를 참고하세요.

## 빌드

Windows + WebUI + Android:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build.ps1
```

macOS host CLI:

```bash
bash ./scripts/build-macos.sh
```

Linux host CLI:

```bash
bash ./scripts/build-linux.sh
```

기본 Rust 검사는 다음과 같습니다.

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

현재 wire protocol은 `1.0`이며 선택 기능은 capability negotiation으로 협상합니다.

## 라이선스

GNU General Public License v3.0 (`GPL-3.0-only`)을 사용합니다. 자세한 내용은 [LICENSE](LICENSE)를 참고하세요.
