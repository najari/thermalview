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
```

기능(MVP): 실시간 열화상, 팔레트 5종, 자동/수동 눈금, 최고·최저 표시, Spot 측정(최대 16개),
셔터 모드 선택(기본 MANUAL)과 FFC(렌즈 가림 확인), 연결 감시·자동 복구, 스냅샷(컬러 PNG + 16비트 원본 PNG + JSON).

## 문서

- [docs/feasibility.md](docs/feasibility.md): 타당성 검토(기술 스택, 위험, 로드맵)
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): 개발 지시서(범위, 구조, 단계별 작업, 완료 기준)
