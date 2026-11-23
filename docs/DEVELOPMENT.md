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
| UI | `eframe`/`egui` 0.37 (기본 wgpu 렌더러) | 검토 결과. 0.37부터 `App::ui(&mut self, ui, frame)`와 `egui::Panel::left/top/bottom` API를 씀 |
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
│  │  ├─ mod.rs               # 상수(VID/PID, 160×120)와 공개 타입
│  │  ├─ worker.rs            # 카메라 작업 스레드: 연결·재연결, 정지 감지, 설정 재적용, 최신 1장 슬롯
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
5. **설정 재적용.** Lepton은 리셋되면 AUTO로 돌아갑니다. 저장된 셔터 모드를 연결 직후와 5초마다 읽어 비교하고, 다르면 다시 씁니다(read-modify-write, 32바이트 객체 중 `shutterMode`만 변경).
10. **스트리밍 중 CCI 명령 최소화.** PureThermal은 Lepton 명령(XU 요청)을 처리하는 동안 영상 프레임을 놓칩니다. 실측: 명령 없이 8.77 fps, 1초마다 4개 명령이면 8.1~8.3 fps(불량 프레임 0, 프레임이 통째로 빠짐). 그래서 상태 폴링은 5초마다 2개(FPA 온도, 셔터 모드 객체)로 제한했고, 이때 8.77 fps로 명령이 없을 때와 같습니다(30초 측정). 또 fps는 USB 요청 없이 프레임 카운터로 계산합니다. 새 상태 항목을 추가할 때는 이 예산을 지킵니다.
11. **제어 전송 타임아웃.** libuvc 원본은 제어 전송 타임아웃이 0(무한)이라, 펌웨어가 응답하지 않으면 스레드와 프로세스 종료가 영원히 멈추고 프로세스가 커널에 남아 장치를 붙잡았습니다(USB 재연결 전까지 `Access denied`). vendor libuvc에 2초 타임아웃을 넣었습니다(`PATCHES.md` 3번).
6. **XU 명령 매핑.** unit = 모듈(명령ID & 0x3F00): AGC 0x0100→3, SYS 0x0200→6, VID 0x0300→7, OEM 0x0800→4, RAD 0x0E00→5. control = ((명령ID & 0xFF) >> 2) + 1. Lepton enum/bool은 전송 시 4바이트. "run" 명령은 control 번호 1바이트를 SET_CUR로 보냅니다.
7. **온도 변환.** 연결 시 RAD TLinear 활성(0x4EC0)과 해상도(0x4EC4: 0=0.1 K, 1=0.01 K)를 읽습니다. TLinear가 꺼져 있으면 "원시값(온도 아님)"을 표시하고 모든 측정값을 `cnt` 단위로 보여 줍니다. 섭씨 = raw × 해상도 − 273.15.
8. **USB 제어와 스트림의 동시성.** XU 명령은 스트리밍 중에도 같은 핸들로 보냅니다. 모든 USB 호출(스트림 시작·정지, XU 명령)은 카메라 작업 스레드 하나에서만 하고, UI는 명령 채널(`Command`)로 요청합니다. 그래서 잠금 없이 직렬화됩니다.
9. **스트림 정지 대응.** 첫 프레임은 8초(Lepton 부팅 대기), 그 뒤로는 2초 동안 프레임이 없으면 장치를 닫고 1초 뒤 다시 엽니다. 3회 연속 프레임을 못 받거나 USB 전송이 끝나지 않으면(`has_stuck_transfers`) USB 재연결 안내를 띄웁니다. "수신 중" 표시는 프레임이 실제로 도착한 뒤에만 합니다. USB 종료 경로를 고치면 `examples/churn`을 디버거 아래에서 돌려 힙 손상이 없는지 확인합니다(`PATCHES.md` 4~9번).

## 5. 스레드와 데이터 흐름

- **libuvc 콜백 스레드:** 프레임 크기(38,400바이트)와 형식을 확인하고 `Vec<u16>`으로 복사해 `Arc<Mutex<Option<ThermalFrame>>>`(최신 1장 슬롯)에 넣은 뒤 `egui::Context::request_repaint()`를 호출합니다. 콜백 안에서는 할당 외의 무거운 일을 하지 않습니다.
- **카메라 작업 스레드(`camera/worker.rs`):** 연결·재연결, 타임아웃 감지, 셔터 모드 재적용, 상태 폴링(1초 간격 FPA 온도 등), UI 명령 실행.
- **UI 스레드:** 슬롯에서 프레임을 가져와 통계·팔레트 변환 후 `TextureHandle::set`(NEAREST 필터)으로 갱신합니다. 프레임이 없어도 상태 갱신을 위해 0.5초마다 다시 그립니다.
- **저장:** 스냅샷 파일 3개는 합쳐 50 KB 정도라 UI 스레드에서 바로 씁니다. 시퀀스 녹화(단계 3)는 별도 스레드로 합니다.

## 6. 단계별 작업

### 단계 1 — 기술 검증 (관문: 초당 8.5장 이상)

1. `libuvc-sys`: vendor 복사, pthread shim, `build.rs`, 최소 FFI. `cargo build`가 MSVC에서 경고 외 오류 없이 통과.
2. `examples/grab.rs`: 장치 열기 → Y16 스트림 5초 → 프레임 수와 fps 출력.
3. 실제 장비로 fps 측정. 8.5 미만이면 원인을 기록하고 방식 2(libusb1-sys 직접 구현)를 검토합니다.

### 단계 2 — MVP (관문: 1시간 연속 동작)

F1~F9를 순서대로 구현합니다. 각 기능은 별도 커밋으로 하고, 커밋 메시지에 기능 ID를 적습니다.

### 진행 현황 (2026-11-22)

| 항목 | 상태 | 근거 |
| --- | --- | --- |
| 단계 1 관문 | 통과 | `examples/grab` 20초: 8.75 fps, 불량 프레임 0 |
| F1 수신 | 완료 | 앱에서 8.7 fps |
| F2 팔레트, F3 범위, F4 최고·최저 | 완료 | 화면 확인 |
| F5 Spot 측정 | 완료 | 클릭 추가·우클릭 삭제 확인 |
| F6 Lepton 제어 | 구현 | 셔터 모드 표시·변경, FFC 확인 대화상자. FFC 실행은 장비에서 미시험 |
| F7 설정 유지 | 구현 | `%APPDATA%\thermalview\config.json`. 셔터 모드 5초 재적용. 종료 후 재시작 시 측정점 복원 확인 |
| F8 연결 감시 | 구현, 일부 시험 | 닫기·다시 열기 150회(`examples/churn`, 디버거 힙 검사): 충돌·힙 손상·멈춘 전송 0건. 물리적 USB 분리는 미시험(시험 절차 6) |
| F9 스냅샷 | 완료 | 16비트 PNG에서 다시 계산한 Spot 값이 JSON과 소수 셋째 자리까지 일치 |
| 1시간 연속 동작 | 통과 | 4차(2026-11-23, 커밋 `3546877`, `examples/soak 60`): 31,070프레임, 평균 8.63 fps, 가장 느린 30초 구간 8.13 fps, 정지 0회, 셔터 모드 재적용 0회. 이전 기록: 1차(8분 중단) 평균 약 8.0 fps, 원인은 CCI 폴링(규칙 10). 2차: 6분째 PC 재부팅. 3차(60분): 평균 8.63 fps지만 37분에 정지 1회로 기준 미달. 이때 상위 VIA USB 허브(2109:0812)와 그 아래 장치가 함께 재연결됐고 Lepton은 재부팅되지 않음. 앱은 약 5초 만에 자동 복구 |

### 단계 3 — 측정 도구

| 항목 | 상태 | 구현 |
| --- | --- | --- |
| Box/Line 측정 | 구현 | `thermal/shapes.rs`: 영역 최고·최저·평균, Bresenham 선. 드래그로 그리기, 끌어서 이동, 영역 테두리·모서리와 선 끝점으로 크기 조절, 우클릭 삭제. 최대 8개. `app/view.rs`의 헤드리스 UI 테스트(실제 egui 입력 이벤트)로 검증 |
| 선 프로파일 | 구현 | 그래프 패널 "선 프로파일" 탭 |
| 히스토그램 | 구현 | `thermal/histogram.rs`, 현재 눈금 범위를 64칸으로, 막대 색은 팔레트 |
| 시간-온도 그래프 | 구현 | `thermal/history.rs` (30분 보관), 1/5/30분 창, 최고·최저·평균·중앙·Spot |
| 측정값 CSV | 구현 | `storage/csv_log.rs`, 프레임마다 1행, UTF-8 BOM(엑셀 호환). Spot과 Box/Line(최고·최저·평균) 열은 기록 시작 시점 기준 |
| 온도 시퀀스 녹화·재생 | 구현 | `storage/sequence.rs` `.tvseq` v2: 무손실 압축(왼쪽 화소 예측 + 바이트 평면 + deflate). 실제 프레임 259장 비트 일치, 2.05:1, 약 0.58 GB/시간, 프레임당 인코딩 0.19 ms(`examples/seqcheck`). 쓰기 스레드 분리, 이전 v1 파일도 읽음. 재생 막대: 재생·일시정지, 프레임 이동, 0.25~8배속. 재생 중에도 측정·그래프·스냅샷·CSV 사용 가능 |

위 항목은 2026-11-23 실제 카메라로 화면에서 조작해 확인했습니다. CSV는 프레임 번호가 빠짐없이 이어지고(111행), `.tvseq`는 화면에 표시된 프레임 수(126)와 일치합니다.

v2 압축과 드래그 편집은 위 자동 시험으로 확인했고, 앱 화면에서 직접 조작한 것은 영역 이동까지입니다.

남은 일: USB 물리 분리 시험, 렌즈를 가린 FFC 시험, 라이선스·배포 결정(10절).

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

- 라이선스(MIT/Apache-2.0 이중 라이선스 권장, GPL 코드를 가져올 경우 GPL-3.0). 주의: 현재 정적 링크하는 libusb는 LGPL-2.1이므로, 실행 파일을 배포하려면 libusb를 DLL로 분리하거나 LGPL의 재링크 조건을 충족해야 합니다
- 외부 배포 여부(배포한다면 방식 3 우선순위 상승)
- 2차 범위 우선순위
