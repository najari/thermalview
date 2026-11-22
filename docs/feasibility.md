# Rust + egui 열화상 뷰어 개발 타당성 검토 (ThermoVision 유사)

2026-11-22 작성. 원본(편집 가능): <https://claude.ai/artifact/QhBexLGpRdaowwmyhi2rKW>

## 요약

**결론: 만들 수 있습니다.** 다만 범위를 ThermoVision 전체가 아니라 "PureThermal + Lepton 3.5 실시간 뷰어와 측정·기록 도구"로 잡을 때 현실적입니다. 1인 기준으로 MVP는 3~4주, 측정·녹화까지 갖춘 실사용 버전은 2~3개월로 추정합니다.

- **가장 어려운 부분은 이미 풀었습니다.** Windows에서 WinUSB로 Lepton Y16 영상을 받고 Lepton 설정을 제어하는 경로를 이번 작업에서 C(libuvc)로 검증했습니다. Rust에서는 이 C 코드를 FFI로 감싸거나 같은 절차를 다시 구현하면 됩니다.
- **egui는 이 용도에 충분합니다.** 160×120 영상을 초당 약 9장 그리는 정도는 GPU 텍스처 갱신으로 부담이 거의 없고, 그래프(egui_plot)와 측정 도구 UI도 바로 만들 수 있습니다.
- **ThermoVision의 큰 부분은 20종이 넘는 제조사별 파일 형식 지원입니다.** 이것까지 따라가면 기간이 크게 늘어나므로, 1차 범위에서는 빼고 필요한 형식만 나중에 추가하는 것을 권합니다.

권장 접근: libuvc(이번에 패치한 것)를 정적으로 링크한 Rust 래퍼로 시작하고, 동작이 안정되면 순수 Rust USB(nusb) 구현으로 바꿀지 판단합니다.

## ThermoVision 분석

ThermoVision은 여러 제조사의 열화상 카메라와 파일을 한 프로그램에서 보고 분석하는 Windows용 도구입니다. 핵심 가치는 "카메라·파일 형식의 폭"과 "측정·분석 창들"입니다. FLIR Lepton/PureThermal은 지원 목록에 없습니다.

| 항목 | 내용 |
| --- | --- |
| 최신 버전 | 1.13.0.0 (2025-02-05). 2.0은 미출시, 미리보기 빌드 1.13.0.8만 공개 |
| 기술 | C# WinForms, .NET Framework 4.0, AForge.Video(DirectShow 웹캠). 태블릿용 창은 WPF |
| 플랫폼 | Windows 전용(설치 없는 ZIP 배포) |
| 라이선스 | 프리웨어. 1.12부터 소스 공개라고 밝혔지만, GitHub 저장소에 라이선스 파일이 없음(재사용 조건 불명) |
| 규모 | C# 약 2.8 MB, 소스 410개 파일, 카메라/장치 패널 20여 개 |
| 실시간 카메라 | Seek Thermal(WinUSB), Thermal Expert Q1, DIY Thermocam(시리얼), FLIR(UVC, RNDIS), FLIR One(앱 중계), Optris Pi 640, 범용 TCamDll, 웹캠 |
| 파일 가져오기 | FLIR .jpg/.seq/.img, Testo .bmt, Seek .tiff, Uni-T .bmp, Jenoptik .irb, Bosch GTC, DJI Mavic 2, NEC/Keysight, InfiRay C210 .irg, Vevor .irg, Hikvision 등 20여 형식 |

소스 구조에서 확인한 분석 기능은 다음과 같습니다(파일 이름 기준이라 세부 동작은 미확인).

- **측정 도구**: Spot, Box, Line, DeltaLine. 측정표(MeasTable), 격자 측정(MeasGrid), 선 프로파일(Lines), 시간 그래프(Plot)
- **표시**: 팔레트 편집(색 깊이 조절), 히스토그램, 눈금 범위 모드 9종(최대/최소를 자동·수동·고정으로 각각 선택), 회전
- **영상 합성**: 웹캠 영상과 열화상 블렌딩·윤곽선 오버레이(frmVisual)
- **보정**: 2점 보정, Planck 보정(PlanckCal), 범용 IR 디코더
- **기록·처리**: 열화상 시퀀스(온도/원시값/float), 일괄 처리, 보고서, 이미지 브라우저
- **자동화·연동**: 온도 스위치(TempSwitch), TCP 서버, Telnet, FTP, 명령줄 옵션(-CAM, -FS, -LOAD_TCS= 등)

출처: [ThermoVision 홈페이지](https://www.joe-c.de/software/thermovision), [지원 카메라](https://www.joe-c.de/software/thermovision/kameras), [버전 1.13.0.0](https://www.joe-c.de/software/thermovision/versionen/1-13-0-0), [GitHub JoeC-de/ThermoVision_JoeC](https://github.com/JoeC-de/ThermoVision_JoeC)

## 대상 하드웨어와 확인된 사실

이번 GetThermal 빌드와 진단 작업에서 실제 장비로 확인한 내용입니다. 새 프로그램은 이 사실을 그대로 설계 조건으로 삼아야 합니다.

| 항목 | 확인 내용 |
| --- | --- |
| 보드 | PureThermal(BOOT/RST 버튼 있는 PT2 계열), USB 1E4E:0100, 펌웨어 v1.3.1(공장 v1.3.0에서 업데이트) |
| 센서 | FLIR Lepton 3.5, 부품번호 500-0771-01, 160×120, Lepton 펌웨어 3.3.26. 셔터 하우징 없음 |
| 영상 형식 | Y16(16비트 온도값, 0.01 K 단위), 160×120, 프레임당 38,400바이트, 초당 약 8.8장 |
| 전송 | USB isochronous, 패킷 642바이트(헤더 2 + 데이터 640 = 영상 2줄) |
| 드라이버 | 복합 장치 상위 노드(Composite Parent)에 WinUSB. 이 상태에서는 Windows 카메라 앱·OpenCV가 장치를 못 봄 |
| Lepton 제어 | UVC 확장 유닛(XU) 3~7번으로 AGC/OEM/RAD/SYS/VID 명령 전달. control id = (명령ID & 0xFF) / 4 + 1 |

설계에 반영해야 할 주의점입니다.

- **인터페이스 점유 순서**: Windows에서는 스트리밍 인터페이스(1)를 먼저 claim한 뒤 협상(probe) 요청을 보내야 합니다. 순서가 바뀌면 libusb가 인터페이스 0 점유까지 놓쳐 `Access denied`가 납니다. 이번에 libuvc를 이 점 때문에 수정했습니다.
- **장치 단독 점유**: WinUSB는 같은 장치를 두 번 열 수 없습니다. USB 문자열(펌웨어 버전 등)은 장치를 열기 전에 읽어야 합니다.
- **셔터 없음**: FFC는 렌즈를 가린 상태에서만 해야 하고, 자동 FFC는 꺼야(MANUAL) 잔상이 생기지 않습니다.
- **설정 초기화**: Lepton은 리셋되면 설정이 기본값(AUTO)으로 돌아갑니다. 프로그램이 선택값을 저장하고 주기적으로 다시 적용해야 합니다.
- **스트림 정지**: 영상 데이터가 아예 오지 않는 상태가 두 번 관찰됐습니다(한 번은 DFU 재시작 직후). USB를 다시 꽂거나 FFC 후 회복됐습니다. 감시와 재연결 처리가 필요합니다.

## Rust 기술 스택 검토

UI와 영상 처리는 검증된 크레이트로 충분합니다. 유일한 공백은 **USB isochronous 영상 수신**입니다. 순수 Rust USB 라이브러리가 아직 이 전송 방식을 지원하지 않습니다.

| 역할 | 크레이트 | 버전(2026-11) | 판단 |
| --- | --- | --- | --- |
| UI 프레임워크 | [eframe / egui](https://crates.io/crates/eframe) | 0.37.0 | 적합. 주간 누적 다운로드 수천만, 활발히 갱신 |
| 도킹 창 | egui_dock | - | ThermoVision식 다중 창(측정표, 그래프 등)에 적합 |
| 그래프 | [egui_plot](https://crates.io/crates/egui_plot) | 0.38.0 | 히스토그램, 시간-온도 그래프, 선 프로파일 |
| USB(순수 Rust) | [nusb](https://crates.io/crates/nusb) | 0.2.9 | **isochronous 미지원.** 제어 전송만 쓸 수 있음([이슈 #47](https://github.com/kevinmehall/nusb/issues/47)) |
| USB(libusb) | [rusb](https://crates.io/crates/rusb) / libusb1-sys | 0.9.4 / 0.7.0 | rusb는 isochronous를 감싸지 않음. libusb1-sys의 원시 함수로는 가능(unsafe) |
| UVC | [uvc](https://crates.io/crates/uvc) | 0.2.0 | libuvc 래퍼지만 2021년 이후 갱신 없음. 원본 libuvc라 Windows 패치도 없음 |
| OS 카메라 | [nokhwa](https://crates.io/crates/nokhwa) | 0.10.11 | Windows Media Foundation 경유. WinUSB 없이 동작하지만 Y16 수신과 Lepton 제어는 미검증 |
| 이미지 저장 | [image](https://crates.io/crates/image), [tiff](https://crates.io/crates/tiff) | 0.25.10, 0.11.3 | PNG 스냅샷, 16비트 TIFF로 온도값 원본 저장 |
| 동영상 | [ffmpeg-next](https://crates.io/crates/ffmpeg-next) | 9.1.0 | 컬러 영상 MP4 녹화. FFmpeg DLL 배포 필요 |
| 설정 | serde, directories | - | 팔레트·측정점·셔터 모드 저장 |

**선행 사례:** [thermal-cat](https://github.com/alufers/thermal-cat)은 InfiRay P2 Pro용 Rust + egui 뷰어입니다(GPL-3.0, eframe + egui_dock + egui_plot + nokhwa + ffmpeg-next). 마커, 최소/최대값 표시, 히스토그램, 시간-온도 그래프, 팔레트, 캡처를 이미 구현했습니다. egui로 이 정도 앱을 만들 수 있다는 근거이고, UI 구조를 참고할 만합니다. 단, GPL이라 코드를 가져오면 새 프로그램도 GPL이 됩니다.

**영상 수신 방식 세 가지**

1. **libuvc FFI (권장)**: 이번에 패치한 libuvc를 `cc` 크레이트로 함께 빌드해 정적 링크합니다. Windows에서 이미 초당 8.8장으로 검증됐고, Lepton 제어(XU)도 같은 핸들로 처리합니다. 단점은 C 빌드 의존성(libusb, pthread)과 WinUSB 드라이버 교체입니다.
2. **libusb1-sys로 직접 구현**: UVC 협상과 payload 조립(헤더 해석, FID/EOF 처리)을 Rust로 직접 작성합니다. Lepton 하나만 대상이면 수백 줄 규모로 추정합니다. C 코드는 없어지지만 unsafe와 콜백 처리가 늘어납니다.
3. **OS 카메라 드라이버(nokhwa)**: WinUSB 교체가 필요 없고 다른 카메라 앱과도 공존합니다. 대신 Media Foundation이 Y16을 손상 없이 넘겨주는지 확인되지 않았고(이번에 OpenCV DirectShow로는 깨진 영상이 나옴), Lepton 제어는 Windows `IKsControl` 확장 유닛 API를 따로 구현해야 합니다.

## 아키텍처 제안

카메라 수신, 화면 그리기, 파일 저장을 서로 다른 스레드로 나눕니다. USB가 멈춰도 창은 응답하고, 녹화가 느려도 영상이 끊기지 않습니다.

```
 PureThermal ──USB iso──▶ [캡처 스레드: libuvc 콜백]
                              │ 최신 프레임 1장 (오래된 것은 버림)
                              ▼
                         [UI 스레드: eframe]  ──명령──▶ [저장 스레드: PNG/TIFF/시퀀스]
                              │
                              └─ Lepton 제어(XU) ── Mutex ── 같은 USB 핸들
```

캡처 스레드는 최신 프레임 한 장만 넘기므로(오래된 프레임은 버림), UI가 느려도 지연이 쌓이지 않습니다. Lepton 제어 명령은 영상과 같은 USB 핸들을 쓰므로 하나의 잠금(Mutex)으로 순서를 맞춥니다.

모듈 구성(크레이트 내부):

- `uvc_sys` — 패치한 libuvc를 `cc`로 빌드하는 FFI 계층
- `lepton` — 장치 열기, Y16 스트림, XU 명령(FFC, 셔터 모드, 게인, SBNUC)
- `thermal` — 온도 프레임 타입, 눈금 범위, 팔레트, 측정 계산
- `storage` — 스냅샷, 온도 시퀀스, 동영상
- `app` — eframe 앱, egui_dock 창 배치, 설정 저장

## 기능별 구현 난이도

측정·표시 기능은 대부분 난이도 하~중입니다. 어려운 것은 USB 수신, 제조사별 파일 형식, 웹캠 정합(fusion)입니다. 공수는 Rust 경험 있는 1인 기준 추정치입니다.

| 기능 | Rust 구현 방법 | 난이도 | 공수(일) | 1차 범위 |
| --- | --- | --- | --- | --- |
| Lepton Y16 실시간 수신 | libuvc FFI + 수신 스레드, 채널로 프레임 전달 | 상 | 5~8 | 예 |
| Lepton 제어(FFC, 셔터 모드, 게인, SBNUC) | XU GET/SET_CUR 래퍼, 명령 ID → control id 변환 | 중 | 3~5 | 예 |
| 연결 감시·재연결, 설정 재적용 | 프레임 타임아웃 감지, 장치 재열기, 저장된 설정 적용 | 중 | 2~3 | 예 |
| 팔레트·색 입히기 | 256색 LUT, CPU에서 RGBA 변환 후 egui 텍스처 갱신 | 하 | 1~2 | 예 |
| 눈금 범위(자동/수동/고정) | 프레임 최소·최대 + 사용자 고정값, 히스테리시스 | 하 | 1~2 | 예 |
| Spot / Box / Line 측정 | 영상 좌표 변환, 드래그 편집, 영역 평균·최대·최소 | 중 | 4~6 | 예 |
| 선 프로파일·히스토그램·시간 그래프 | egui_plot | 하 | 2~3 | 예 |
| 측정표(CSV 내보내기) | egui_extras 테이블 + csv | 하 | 1~2 | 예 |
| 스냅샷(컬러 PNG + 온도 원본) | image(PNG), tiff(16비트) 또는 자체 형식 + JSON 메타 | 하 | 1~2 | 예 |
| 온도 시퀀스 녹화·재생 | 프레임별 Y16 저장(압축 선택), 타임라인 UI | 중 | 4~6 | 2차 |
| 컬러 동영상(MP4) | ffmpeg-next, FFmpeg DLL 번들 | 중 | 3~4 | 2차 |
| 온도 경보(TempSwitch) | 임계값 조건 + 소리/시리얼/외부 명령 | 하 | 2 | 2차 |
| 웹캠 오버레이(fusion) | nokhwa 웹캠 수신, 정합(크기·위치 보정), 블렌딩 | 상 | 6~10 | 3차 |
| TCP/원격 제어 | tokio 또는 std::net, 간단한 텍스트 프로토콜 | 중 | 3~4 | 3차 |
| Planck·2점 보정 | 보정 계수 저장, 복사율·반사온도 보정 | 중 | 3~5 | 3차 |
| 타사 카메라(Seek, InfiRay 등) | 카메라별 프로토콜·드라이버 분석 | 상 | 카메라당 5~15 | 제외 |
| 제조사 파일 형식 20여 종 | 형식별 디코더(FLIR jpg는 exif + raw 해석) | 상 | 형식당 2~5 | 제외 |

1차 범위 합계는 약 20~35일입니다. 요약의 "MVP 3~4주"는 이 표의 1차 항목 중 핵심(수신, 제어, 팔레트, Spot 측정, 스냅샷)을 말합니다.

## 위험 요소와 대응

가장 큰 위험은 USB 계층입니다. UI 쪽 위험은 낮습니다.

| 위험 | 영향 | 대응 |
| --- | --- | --- |
| 순수 Rust USB가 isochronous 미지원 | 높음: 영상 수신 불가 | libuvc FFI로 시작. nusb에 isochronous가 들어오면 교체 검토 |
| WinUSB 드라이버 교체 필요 | 중간: 사용자가 Zadig를 직접 실행, 다른 카메라 앱과 공존 불가 | 설치 안내 화면 제공. 장기적으로 libwdi 자동 설치 또는 Media Foundation 경로(방식 3) 검증 |
| libusb Windows 자동 점유 버그 | 높음: 스트리밍 시작 실패 | 이번에 수정한 libuvc 사용(스트리밍 인터페이스를 협상 전에 claim) |
| 스트림 정지(프레임 미수신) | 중간: 화면 멈춤 | 프레임 타임아웃 감지 → 스트림 재시작 → 실패 시 사용자에게 USB 재연결 안내 |
| 셔터 없는 모듈의 FFC 잔상 | 중간: 측정값 오류 | 기본 MANUAL, FFC 버튼에 "렌즈를 가렸나요?" 확인, 설정 주기적 재적용 |
| C 빌드 의존성(libusb, pthread, MinGW/MSVC) | 낮음~중간: 빌드 환경 복잡 | `cc` 크레이트로 libuvc 소스를 같이 컴파일, libusb는 vcpkg 또는 번들 DLL |
| egui 텍스처 갱신 성능 | 낮음: 160×120 RGBA는 프레임당 약 77 KB | `TextureHandle::set`으로 프레임마다 갱신, 확대는 GPU에 맡김 |
| 라이선스 | 중간: 코드 재사용 제한 | ThermoVision은 라이선스 파일이 없어 코드 재사용 불가(기능만 참고). thermal-cat은 GPL-3.0. libuvc는 BSD 계열이라 포함 가능 |
| 크로스플랫폼 | 낮음: 우선 Windows만 | egui와 libuvc는 Linux/macOS도 지원. Linux에서는 드라이버 교체 없이 동작하므로 나중에 확장 쉬움 |

## 로드맵

기술 검증 1주 후 MVP와 측정 도구를 더해 약 7~9주면 일상적으로 쓸 수 있는 버전이 됩니다. 확장 단계는 실제로 써 본 뒤 필요한 것만 고릅니다.

| 단계 | 기간 | 내용 | 다음 단계로 가는 관문 |
| --- | --- | --- | --- |
| 1. 기술 검증 | 1주 | libuvc FFI 빌드, eframe 창에 Y16 영상 표시 | 초당 8.8장 유지 |
| 2. MVP | 3~4주 | 수신·제어·팔레트·Spot 측정·스냅샷·재연결 | 1시간 연속 동작 |
| 3. 측정 도구 | 3~4주 | Box/Line, 그래프, 측정표, 시퀀스 녹화 | 실사용 피드백 |
| 4. 확장 | 4주+ | 동영상, 경보, 원격 제어, 보정, 웹캠 합성 | - |

기술 검증에서 초당 8.8장이 유지되지 않으면 영상 수신 방식을 다시 고르고, MVP가 1시간 연속 동작을 넘지 못하면 재연결 처리부터 보강합니다.

## 권고와 다음 단계

개발을 진행하되, ThermoVision 복제가 아니라 **이 장비(PureThermal + 셔터 없는 Lepton 3.5)에 맞춘 측정 도구**를 목표로 삼을 것을 권합니다. ThermoVision이 다루지 않는 Lepton/PureThermal을 제대로 지원하는 것 자체가 차별점입니다.

먼저 할 일입니다.

- [ ] **기술 검증(2~3일)**: Rust 크레이트가 패치한 libuvc를 `cc`로 빌드하고, eframe 창에 Y16 영상을 팔레트로 띄우는 최소 예제. 초당 8.8장이 유지되면 방식 1로 확정
- [ ] **방식 3 확인(1일)**: 드라이버를 원래대로 돌린 상태에서 nokhwa(Media Foundation)로 Y16이 온전히 들어오는지 시험. 되면 Zadig 없는 배포가 가능해짐
- [ ] **라이선스 결정**: thermal-cat 코드를 참고만 할지(독자 라이선스), 가져다 쓸지(GPL-3.0) 선택
- [ ] **1차 범위 확정**: 기능 표의 "1차 범위: 예" 항목에서 빼거나 더할 것 정리

남은 질문: 이 프로그램을 다른 사람에게 배포할 계획인가요? 배포한다면 WinUSB 설치 절차가 가장 큰 장벽이라 방식 3의 우선순위가 올라갑니다.

### 출처

- [ThermoVision 홈페이지](https://www.joe-c.de/software/thermovision) · [지원 카메라](https://www.joe-c.de/software/thermovision/kameras) · [버전 목록](https://www.joe-c.de/software/thermovision/versionen) · [1.13.0.0](https://www.joe-c.de/software/thermovision/versionen/1-13-0-0) · [명령줄 옵션](https://www.joe-c.de/software/thermovision/shellbefehle)
- [ThermoVision 소스(GitHub)](https://github.com/JoeC-de/ThermoVision_JoeC)
- [thermal-cat(GitHub)](https://github.com/alufers/thermal-cat)
- [nusb isochronous 이슈 #47](https://github.com/kevinmehall/nusb/issues/47) · [PR #233(미병합 종료)](https://github.com/kevinmehall/nusb/pull/233)
- 크레이트 버전: [crates.io](https://crates.io) 조회(2026-11-22)
- 하드웨어 사실: GetThermal 빌드 세션의 직접 측정(grab_raw, lepton_query, stream_monitor)
