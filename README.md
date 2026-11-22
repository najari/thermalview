# ThermalView

PureThermal + FLIR Lepton 3.5용 열화상 뷰어·측정 도구입니다. Rust와 [egui](https://github.com/emilk/egui)로 만듭니다.
[ThermoVision](https://www.joe-c.de/software/thermovision)과 비슷한 측정 기능을 목표로 하되, ThermoVision이 지원하지 않는 Lepton/PureThermal에 맞춥니다.

> 개발 초기 단계입니다. 진행 상황은 [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)를 보세요.

## 빌드와 실행

사전 조건: Rust(MSVC 툴체인), Visual Studio Build Tools, PureThermal에 WinUSB 드라이버(Zadig로 Composite Parent에 설치).

```powershell
cargo build --release
.\target\release\thermalview.exe          # 앱
cargo run --release --example grab        # 수신 속도 확인
cargo run --release --example soak 60     # 60분 연속 동작 시험
cargo run --release --example churn 150   # 열기·수신·닫기 반복 시험
cargo run --release --example seqcheck 30 # 압축 녹화가 무손실인지 실제 프레임으로 확인
```

기능(MVP): 실시간 열화상, 팔레트 5종, 자동/수동 눈금, 최고·최저 표시, Spot 측정(최대 16개),
셔터 모드 선택(기본 MANUAL)과 FFC(렌즈 가림 확인), 연결 감시·자동 복구, 스냅샷(컬러 PNG + 16비트 원본 PNG + JSON).

## 문제 해결

| 증상 | 조치 |
| --- | --- |
| "다른 프로그램이 사용 중" | GetThermal 등 카메라를 쓰는 프로그램을 닫습니다(WinUSB는 한 프로그램만 열 수 있음) |
| "카메라 찾는 중"이 계속됨 | WinUSB 드라이버(Zadig, Composite Parent)를 확인합니다 |
| USB를 다시 꽂아도, 다른 포트에 꽂아도 인식되지 않음 | Windows가 이전 장치 기록을 정리하지 못한 상태입니다. PC를 재부팅하거나, 관리자 권한으로 `pnputil /remove-device "USB\VID_1E4E&PID_0100\<시리얼>"` 실행 후 다시 꽂습니다 |
| 가끔 영상이 몇 초 끊겼다가 돌아옴 | USB 허브가 재연결되는 경우입니다(앱이 자동 복구). 허브를 거치지 않고 PC 본체 포트에 연결하면 줄어듭니다 |
| 재부팅 후 잔상이 생김 | 재부팅하면 Lepton이 AUTO 셔터 모드로 돌아갑니다. 앱이 연결되면 저장된 모드(기본 MANUAL)를 다시 적용합니다 |

## 문서

- [docs/feasibility.md](docs/feasibility.md): 타당성 검토(기술 스택, 위험, 로드맵)
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): 개발 지시서(범위, 구조, 단계별 작업, 완료 기준)
