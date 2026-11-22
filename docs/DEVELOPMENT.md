# ThermalView 개발 지시서

작성 2026-11-22. 근거 문서: [feasibility.md](feasibility.md).
이 문서는 사람 개발자와 코딩 에이전트가 같은 기준으로 작업하기 위한 지시서입니다. 설계 결정이 바뀌면 이 문서를 먼저 고치고 코드를 바꿉니다.

## 1. 목표와 범위

**목표:** PureThermal + 셔터 없는 FLIR Lepton 3.5를 위한 Windows용 실시간 열화상 뷰어·측정 도구. 단일 실행 파일(`thermalview.exe`)로 배포합니다.

**1차 범위(MVP):**

| ID | 기능 | 완료 기준 |
| --- | --- | --- |
| F1 | Y16 실시간 수신 | 160×120 Y16을 초당 8.5장 이상으로 받는다 |
| F2 | 팔레트 표시 | Ironbow, Rainbow, Grayscale, White-hot, Black-hot 중 선택해 표시 |
| F3 | 눈금 범위 | 자동(프레임 최소·최대) / 수동(사용자 고정값). 색 막대에 온도 표시 |
| F4 | 최고·최저·중앙점 표시 | 영상 위 표식과 수치 |
| F5 | Spot 측정 | 클릭으로 측정점 추가·삭제, 3×3 평균 온도 표시 |
| F6 | Lepton 제어 | FFC 실행(렌즈 가림 확인), FFC 셔터 모드(MANUAL/AUTO/EXTERNAL), 상태 표시(FPA 온도, 마지막 FFC 이후 시간) |
| F7 | 설정 유지 | 셔터 모드·팔레트·범위·측정점을 저장, 시작 시 복원, 셔터 모드는 주기적으로 재적용 |
| F8 | 연결 감시 | 2초간 프레임이 없으면 스트림 재시작, 실패 시 재연결 루프와 "USB를 다시 꽂으세요" 안내 |
| F9 | 스냅샷 | 컬러 PNG + 온도 원본(16비트 PNG, 단위 0.01 K) + JSON 메타 |

**2차 이후:** Box/Line 측정, 히스토그램, 시간-온도 그래프, 측정표 CSV, 온도 시퀀스 녹화·재생, MP4, 온도 경보, 원격 제어, 보정, 웹캠 합성. 타사 카메라와 제조사 파일 형식은 범위 밖입니다.

## 2. 확정된 기술 결정

| 항목 | 결정 | 이유 |
| --- | --- | --- |
| 언어·툴체인 | Rust stable, `x86_64-pc-windows-msvc` | 개발 PC 기본 툴체인. MinGW 불필요 |
| UI | `eframe`/`egui` 0.37 (glow 렌더러) | 검토 결과 |
| 영상 수신 | 방식 1: 패치한 libuvc를 `cc`로 정적 빌드해 FFI | Windows에서 검증된 경로 |
| libusb | `libusb1-sys`의 `vendored` 기능(정적 링크) | DLL 배포 불필요 |
| pthread | `crates/libuvc-sys/compat/pthread.h` 자체 shim(Win32 SRWLOCK + CONDITION_VARIABLE) | libuvc가 쓰는 함수가 13개뿐 |
| 바인딩 | 손으로 쓴 최소 FFI(`bindgen` 미사용) | libclang 설치 의존 제거 |
| 설정 저장 | `serde` + JSON, `%APPDATA%\thermalview\config.json` | 이식성, 사람이 읽을 수 있음 |
| 이미지 저장 | `image` (PNG 8비트 컬러, 16비트 그레이) | 16비트 PNG로 온도 원본을 손실 없이 저장 |
| 라이선스 | **미정**. 결정 전까지 GPL 코드(thermal-cat 등)는 복사하지 않는다 | 사용자 결정 사항 |

방식 3(nokhwa/Media Foundation, Zadig 불필요)은 MVP 이후 별도 실험으로 검증합니다. 캡처 계층은 `trait`로 분리해 나중에 백엔드를 추가할 수 있게 합니다.

## 3. 저장소 구조

```
thermalview/
├─ Cargo.toml                 # workspace + 앱 크레이트
├─ crates/
│  └─ libuvc-sys/             # FFI 계층 (unsafe는 여기와 src/camera/에만)
│     ├─ build.rs             # libuvc C 소스 + shim 컴파일, libusb1-sys에 링크
│     ├─ compat/              # pthread.h, sys/time.h shim (MSVC 전용)
│     ├─ vendor/libuvc/       # GetThermal 세션에서 패치한 libuvc (BSD, LICENSE.txt 유지)
│     └─ src/lib.rs           # extern "C" 선언과 구조체
├─ src/
│  ├─ main.rs                 # 진입점, 로깅, eframe 실행
│  ├─ camera/
│  │  ├─ mod.rs               # Camera trait, Frame 전달(최신 1장), 감시 스레드
│  │  ├─ uvc.rs               # libuvc 안전 래퍼(Context/Device/Handle/Stream)
│  │  └─ lepton.rs            # XU 명령(CCI): 명령ID → (unit, control) 변환과 타입별 get/set
│  ├─ thermal/
│  │  ├─ frame.rs             # ThermalFrame(Y16 + 단위), 온도 변환, 통계
│  │  ├─ palette.rs           # 256색 LUT
│  │  ├─ range.rs             # 눈금 범위 모드
│  │  └─ measure.rs           # Spot(이후 Box/Line)
│  ├─ storage/snapshot.rs
│  ├─ settings.rs
│  └─ app/                    # egui 화면: 영상 뷰, 제어 패널, 상태 줄
└─ docs/
```

## 4. 하드웨어 규칙 (반드시 지킬 것)

이 규칙들은 실제 장비 디버깅에서 나온 것입니다. 어기면 재현하기 어려운 오류가 납니다.

1. **WinUSB 단독 점유.** 장치는 한 프로세스만 열 수 있습니다. GetThermal 등 다른 프로그램이 열려 있으면 `uvc_open`이 실패하므로 "다른 프로그램이 카메라를 사용 중" 메시지를 보여 줍니다. USB 문자열(제조사, 제품, 시리얼=펌웨어 버전)은 `uvc_open` **전에** `uvc_get_device_descriptor`로 읽습니다.
2. **claim 후 probe.** vendor libuvc의 `uvc_get_stream_ctrl_format_size`는 스트리밍 인터페이스를 먼저 claim하도록 패치되어 있습니다. 이 패치를 되돌리지 않습니다. libuvc를 갱신할 때는 `vendor/libuvc/PATCHES.md`의 변경을 다시 적용합니다.
3. **실패한 시작 후 핸들 유지.** `uvc_start_streaming`이 실패해도 곧바로 `uvc_close`하지 않습니다(GetThermal에서 크래시 원인). 정리는 Handle의 `Drop` 한 곳에서만 합니다.
4. **셔터 없음.** 기본 셔터 모드는 MANUAL입니다. FFC 버튼은 "렌즈를 가렸습니까?" 확인 후에만 명령을 보냅니다. AUTO를 고르면 잔상 경고를 표시합니다.
5. **설정 재적용.** Lepton은 리셋되면 AUTO로 돌아갑니다. 저장된 셔터 모드를 연결 직후와 3초마다 읽어 비교하고, 다르면 다시 씁니다(read-modify-write, 32바이트 객체 중 `shutterMode`만 변경).
6. **XU 명령 매핑.** unit = 모듈(명령ID & 0x3F00): AGC 0x0100→3, SYS 0x0200→6, VID 0x0300→7, OEM 0x0800→4, RAD 0x0E00→5. control = ((명령ID & 0xFF) >> 2) + 1. Lepton enum/bool은 전송 시 4바이트. "run" 명령은 control 번호 1바이트를 SET_CUR로 보냅니다.
7. **온도 변환.** 연결 시 RAD TLinear 활성(0x4EC0)과 해상도(0x4EC4: 0=0.1 K, 1=0.01 K)를 읽습니다. TLinear가 꺼져 있으면 온도 대신 원시값임을 화면에 표시하고 온도 측정을 비활성화합니다. 섭씨 = raw × 해상도 − 273.15.
8. **USB 제어와 스트림의 동시성.** XU 명령은 스트리밍 중에도 같은 핸들로 보내되, 앱 전체에서 하나의 `Mutex`로 직렬화합니다.
9. **스트림 정지 대응.** 프레임 타임아웃(2초) → stop/start 재시도 1회 → 실패 시 핸들을 닫고 1초 간격으로 다시 찾기. DFU 직후처럼 재연결로도 회복되지 않으면 USB 재연결 안내를 띄웁니다.

## 5. 스레드와 데이터 흐름

- **libuvc 콜백 스레드:** 프레임 크기(38,400바이트)와 형식을 확인하고 `Vec<u16>`으로 복사해 `Arc<Mutex<Option<ThermalFrame>>>`(최신 1장 슬롯)에 넣은 뒤 `egui::Context::request_repaint()`를 호출합니다. 콜백 안에서는 할당 외의 무거운 일을 하지 않습니다.
- **감시 스레드:** 연결·재연결, 타임아웃 감지, 셔터 모드 재적용, 상태 폴링(1초 간격 FPA 온도 등).
- **UI 스레드:** 슬롯에서 프레임을 가져와 통계·팔레트 변환 후 `TextureHandle::set`(NEAREST 필터)으로 갱신합니다.
- **저장:** 스냅샷은 UI 스레드에서 프레임을 복제해 `std::thread::spawn`으로 씁니다.

## 6. 단계별 작업

### 단계 1 — 기술 검증 (관문: 초당 8.5장 이상)

1. `libuvc-sys`: vendor 복사, pthread shim, `build.rs`, 최소 FFI. `cargo build`가 MSVC에서 경고 외 오류 없이 통과.
2. `examples/grab.rs`: 장치 열기 → Y16 스트림 5초 → 프레임 수와 fps 출력.
3. 실제 장비로 fps 측정. 8.5 미만이면 원인을 기록하고 방식 2(libusb1-sys 직접 구현)를 검토합니다.

### 단계 2 — MVP (관문: 1시간 연속 동작)

F1~F9를 순서대로 구현합니다. 각 기능은 별도 커밋으로 하고, 커밋 메시지에 기능 ID를 적습니다.

### 단계 3 — 측정 도구

Box/Line 측정, 히스토그램, 시간-온도 그래프(egui_plot), 측정표 CSV, 온도 시퀀스 녹화·재생.

## 7. 코드 규칙

- `unsafe`는 `crates/libuvc-sys`와 `src/camera/uvc.rs`에만 둡니다. 각 `unsafe` 블록 위에 안전 조건을 주석으로 적습니다.
- 캡처·제어 경로에서 `unwrap`/`expect` 금지. 오류는 `anyhow::Result` 또는 전용 오류 타입으로 UI까지 올려 상태 줄에 표시합니다.
- `thermal/` 모듈은 장치 없이 테스트할 수 있어야 합니다(순수 함수). 팔레트, 범위, 통계, Spot 계산에 단위 테스트를 둡니다.
- 커밋 전: `cargo fmt`, `cargo clippy --all-targets`(경고 0 목표), `cargo test`.
- UI 문자열은 한국어를 기본으로 합니다. 폰트는 Windows의 `malgun.ttf`를 실행 시 읽어 egui에 추가합니다(없으면 기본 폰트).

## 8. 빌드와 실행

```powershell
cargo build --release
.\target\release\thermalview.exe
# 수신 검증
cargo run --release --example grab
```

사전 조건: Visual Studio Build Tools(C 컴파일러), PureThermal에 WinUSB 드라이버(Zadig: Options > List All Devices, "Ignore Hubs or Composite Parents" 해제 후 Composite Parent에 설치).
환경 변수 `LIBUVC_TRACE=1`(프레임) / `2`(패킷)로 vendor libuvc의 진단 출력을 켤 수 있습니다.

## 9. 장비 시험 절차

1. GetThermal 등 카메라를 쓰는 프로그램을 모두 닫습니다.
2. `cargo run --release --example grab` → fps ≥ 8.5 확인.
3. 앱 실행 → 영상, 최고·최저 표시, Spot 측정 확인.
4. 셔터 모드 MANUAL 선택 → 앱 재시작 → MANUAL 유지 확인.
5. 렌즈를 가리고 FFC → 잔상 없음 확인.
6. 스트리밍 중 USB 분리 → 안내 표시 → 다시 연결 → 자동 복구 확인.
7. 스냅샷 → PNG 두 장과 JSON 생성, 16비트 PNG 값/100 − 273.15가 화면 Spot 값과 일치하는지 확인.
8. 1시간 연속 실행 후 fps와 메모리 확인.

## 10. 열린 결정

- 라이선스(MIT/Apache-2.0 이중 라이선스 권장, GPL 코드를 가져올 경우 GPL-3.0)
- 외부 배포 여부(배포한다면 방식 3 우선순위 상승)
- 2차 범위 우선순위
