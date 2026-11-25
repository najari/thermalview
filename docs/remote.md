# 원격 제어 (TCP)

다른 프로그램이 ThermalView에 TCP로 접속해 측정값을 읽고, 측정점·표시·방사율을 바꾸고, 스냅샷·CSV 기록·녹화를 시작할 수 있습니다. 측정 자동화 스크립트나 다른 장비와 연동할 때 씁니다.

## 켜기

왼쪽 패널 맨 아래 **원격 제어**에서 **TCP 원격 제어 사용**을 켭니다.

- 기본 포트는 7700입니다. 포트와 접속 범위는 원격 제어를 끈 상태에서만 바꿀 수 있습니다.
- 기본값은 이 PC에서만 접속할 수 있습니다(127.0.0.1).
- **다른 PC에서 접속 허용**을 켜면 모든 네트워크 주소에서 받습니다. 처음 켤 때 Windows 방화벽이 허용 여부를 물을 수 있습니다.
- **인증이 없습니다.** 다른 PC 접속을 허용하면 같은 네트워크의 누구나 측정값을 읽고 스냅샷·기록을 시작할 수 있습니다. 믿을 수 있는 네트워크에서만 켜세요.
- 설정은 저장되며, 켜 둔 채 종료하면 다음 실행 때 자동으로 다시 엽니다.

## 형식

- 한 줄에 명령 하나. 줄 끝은 LF 또는 CRLF. 대소문자를 가리지 않습니다.
- 명령마다 정확히 한 줄로 응답합니다(CRLF). 성공은 `OK`, 실패는 `ERR 이유`로 시작합니다.
- 값은 `이름=값` 형태로 공백으로 구분합니다. 온도는 소수 둘째 자리, 단위는 `unit`(`C` 또는 TLinear가 꺼진 경우 `cnt`)을 따릅니다.
- 보정(방사율·기준점)이 켜져 있으면 모든 온도는 보정된 값입니다. `FRAME`만 카메라 원시값입니다.
- 파일 경로를 돌려주는 응답은 경로가 줄의 끝까지입니다(경로에 공백이 있을 수 있음).
- 텍스트는 UTF-8입니다. 오류 이유에 Windows의 한국어 오류 메시지가 섞일 수 있습니다.
- 한 줄은 256바이트까지. 넘으면 `ERR line too long` 후 연결을 끊습니다.
- 동시에 4개 연결까지 받습니다. 그 이상은 `ERR too many clients` 후 끊습니다.

## 명령

| 명령 | 응답 예 | 설명 |
| --- | --- | --- |
| `HELP` | `OK commands: ...` | 명령 목록 |
| `STATUS` | `OK state=streaming fps=8.9 frames=1234 fpa=31.00 unit=C source=live csv=off rec=off mp4=off cal=off alarms=0` | 연결 상태(`starting`, `searching`, `busy`, `streaming`, `stalled`, `replug`), 초당 프레임, 받은 프레임 수, 센서 온도, 영상 출처(`live`/`playback`), CSV·녹화·MP4·보정 여부, 발생 중인 경보 수 |
| `MEAS` | `OK seq=170 time=2026-11-25T10:04:16.856+09:00 unit=C max=42.51 min=29.56 mean=33.45 center=33.59 maxpos=3,79 minpos=0,1 S1=33.59 B2.max=... B2.min=... B2.mean=...` | 현재 프레임의 최고·최저·평균·중앙(3×3 평균), 최고·최저 위치, 측정점 `S<id>`, 영역·선 `B<id>`/`L<id>`의 최고·최저·평균 |
| `TEMP x y` | `OK 33.59` | 화소 (x, y) 주변 3×3 평균. x는 0~159, y는 0~119 |
| `SPOT` 또는 `SPOT LIST` | `OK S1=80,60 S3=10,20` | 측정점 목록 |
| `SPOT ADD x y` | `OK id=4` | 측정점 추가(최대 16개) |
| `SPOT DEL id` | `OK` | 측정점 삭제(`3`과 `S3` 모두 가능). 이 측정점을 쓰던 경보 규칙도 지워집니다 |
| `SPOT CLEAR` | `OK` | 측정점 모두 삭제. 번호는 이어서 매깁니다 |
| `PALETTE [이름]` | `OK ironbow` | 팔레트 읽기·바꾸기: `ironbow`, `rainbow`, `whitehot`, `blackhot`, `lava` |
| `RANGE` | `OK auto 29.54 42.51` | 눈금 방식과 지금 쓰는 범위 |
| `RANGE AUTO` / `RANGE lo hi` | `OK manual 20.00 40.00` | 자동 범위 / 수동 범위 |
| `EMISSIVITY [e]` | `OK 0.950` | 방사율 읽기·바꾸기(0.10~1.00). CSV 기록 중에는 바꿀 수 없음 |
| `ALARMS` | `OK enabled=on monitoring=on R1=active R2=clear` | 경보 감시 상태(`on`, `playback`, `nosignal`, `counts`)와 규칙별 상태(`active`, `pending`, `clear`, `off`) |
| `SNAPSHOT` | `OK C:\...\thermal_20261125_100417_368.png` | 스냅샷 저장(컬러 PNG, 16비트 PNG, JSON). 컬러 PNG 경로를 돌려줌 |
| `CSV START` / `CSV STOP` | `OK C:\...\thermal_log_....csv` / `OK rows=86 C:\...` | 측정값 CSV 기록 |
| `REC START` / `REC STOP` | `OK C:\...\thermal_seq_....tvseq` / `OK frames=169 C:\...` | 온도 원본 녹화. 재생 중에는 시작할 수 없음 |
| `MP4 START` / `MP4 STOP` | `OK C:\...\thermal_video_....mp4` / `OK frames=169 C:\...` | 보이는 영상(팔레트, 측정 표시, 색 막대, 시각)을 MP4로 녹화. 재생 중이면 재생 화면을 녹화. 프레임 없이 멈추면 파일을 만들지 않고 `ERR no frames were recorded; no file written` |
| `FRAME` | `OK w=160 h=120 seq=174 k=0.01 data=VHZm...` | 현재 프레임의 원시값. `data`는 little-endian u16을 base64로 인코딩한 값(38,400바이트 → 51,200자). 섭씨 = 값 × k − 273.15. TLinear가 꺼져 있으면 `k=none` |
| `QUIT` | `OK bye` | 연결 종료 |

프레임이 아직 없으면 측정·저장 명령은 `ERR no frame yet`을 돌려줍니다.

**원격으로 할 수 없는 것:** FFC 실행과 셔터 모드 변경(셔터가 없는 Lepton이라 렌즈를 가렸는지 사람이 확인해야 함), 기준점 기록, 영역·선 그리기, 경보 규칙 편집. 이것들은 화면에서 합니다.

## 동작

- 명령은 화면과 같은 상태를 다룹니다. 원격으로 바꾼 측정점·팔레트·범위·방사율은 화면에 바로 보이고 설정에 저장되며, 상태 줄에 "원격: ..." 알림이 뜹니다.
- 창을 최소화해도 응답하고, 그동안에도 CSV 기록·녹화·경보가 모든 프레임에 대해 계속됩니다.
- 응답 시간: 창이 보일 때 약 1~50 ms, 최소화했을 때 약 100 ms.
- 앱이 3초 안에 응답하지 못하면 `ERR application not responding`을 돌려줍니다.
- 재생 중에는 `MEAS`, `FRAME`, `SNAPSHOT`이 재생 중인 프레임을 대상으로 합니다(`STATUS`의 `source=playback`).

## 예

PowerShell:

```powershell
$c = [Net.Sockets.TcpClient]::new('127.0.0.1', 7700)
$s = $c.GetStream()
$r = [IO.StreamReader]::new($s); $w = [IO.StreamWriter]::new($s); $w.AutoFlush = $true
$w.WriteLine('MEAS'); $r.ReadLine()
$c.Close()
```

Python (1초마다 중앙 온도 기록):

```python
import socket, time

s = socket.create_connection(("127.0.0.1", 7700))
f = s.makefile("rw", encoding="utf-8", newline="")

def cmd(line):
    f.write(line + "\n")
    f.flush()
    reply = f.readline().strip()
    if not reply.startswith("OK"):
        raise RuntimeError(reply)
    return reply[3:]

for _ in range(10):
    values = dict(kv.split("=", 1) for kv in cmd("MEAS").split())
    print(values["time"], values["center"])
    time.sleep(1)
```

원시 프레임을 numpy 배열로:

```python
import base64, numpy as np

fields = dict(kv.split("=", 1) for kv in cmd("FRAME").split())
raw = np.frombuffer(base64.b64decode(fields["data"]), dtype="<u2")
raw = raw.reshape(int(fields["h"]), int(fields["w"]))
celsius = raw * float(fields["k"]) - 273.15   # 보정(방사율 등) 적용 전 값
```
