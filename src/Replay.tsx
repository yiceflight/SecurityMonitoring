import { useEffect, useMemo, useRef, useState } from "react";
import { call, native } from "./monitor";
import type { CaptureRecord } from "./monitor";
import "./Replay.css";

const capturedTime = (record: CaptureRecord) => record.capturedAt.slice(0, 19).replace("T", " ");
const timestamp = (record: CaptureRecord) => new Date(record.capturedAt).getTime();
const axisTime = (value: number) => new Date(value).toLocaleString("zh-CN", {
  month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
});

export default function Replay({ available }: { available: boolean }) {
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<{ key: string; records: CaptureRecord[]; error: boolean } | null>(null);
  const requestKey = `${revision}/${available}`;

  useEffect(() => {
    if (!available) return;
    let active = true;
    call<CaptureRecord[]>("list_replay_records").then(
      (records) => { if (active) setResult({ key: requestKey, records, error: false }); },
      () => { if (active) setResult({ key: requestKey, records: [], error: true }); },
    );
    return () => { active = false; };
  }, [available, requestKey]);

  const current = result?.key === requestKey ? result : null;
  const message = !available
    ? !native ? "请在 Windows 客户端中查看本机回放记录" : "正在等待可用的 Windows 客户端"
    : !current ? "正在读取回放记录…"
      : current.error ? "无法读取回放记录，请刷新重试"
        : current.records.length === 0 ? "暂无回放记录" : "";

  return (
    <section className="replay-panel" aria-labelledby="replay-title">
      <div className="replay-heading">
        <h2 id="replay-title">时间回放</h2>
        <button disabled={!available || !current} onClick={() => setRevision((value) => value + 1)}>刷新记录</button>
      </div>
      {message ? <>
        <div className="replay-empty" role={current?.error ? "alert" : "status"}>
          <svg width="54" height="54" viewBox="0 0 54 54" fill="none" aria-hidden="true">
            <rect x="5" y="8" width="44" height="31" rx="5" stroke="currentColor" strokeWidth="2" />
            <path d="M19 47h16M27 39v8M24 18l10 6-10 6V18Z" stroke="currentColor" strokeWidth="2" strokeLinejoin="round" />
          </svg>
          <strong>{message}</strong>
        </div>
        <div className="timeline-placeholder" aria-hidden="true" />
        <p className="replay-hint">有记录后，可拖动下方时间轴查看截图</p>
      </> : current && <ReplayViewer key={requestKey} records={current.records} />}
    </section>
  );
}

function ReplayViewer({ records }: { records: CaptureRecord[] }) {
  const [index, setIndex] = useState(records.length - 1);
  const [cursor, setCursor] = useState(() => timestamp(records[records.length - 1]));
  const [enlarged, setEnlarged] = useState(false);
  const [imageRetry, setImageRetry] = useState(0);
  const [image, setImage] = useState<{ key: string; src: string; state: "loading" | "ready" | "error" } | null>(null);
  const record = records[index];
  const imageKey = `${record.id}/${imageRetry}`;
  const currentImage = image?.key === imageKey ? image : null;
  const ready = currentImage?.state === "ready";
  const times = useMemo(() => records.map(timestamp), [records]);
  const start = times[0], end = times[times.length - 1];

  useEffect(() => {
    let active = true;
    // Scrubbing can select many records in a few milliseconds. Read only after
    // the pointer settles, and discard stale in-flight responses.
    const timer = setTimeout(() => {
      call<string>("get_record_image", { id: record.id }).then(
        (src) => { if (active) setImage({ key: imageKey, src, state: "loading" }); },
        () => { if (active) setImage({ key: imageKey, src: "", state: "error" }); },
      );
    }, 100);
    return () => { active = false; clearTimeout(timer); };
  }, [record.id, imageKey]);

  function seek(value: number) {
    let low = 0, high = times.length - 1;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      if (times[mid] < value) low = mid + 1;
      else high = mid;
    }
    const nearest = low > 0 && value - times[low - 1] <= times[low] - value ? low - 1 : low;
    setIndex(nearest);
    setCursor(value);
  }
  const position = (value: number) => ((value - start) / Math.max(1, end - start)) * 100;
  const ticks = useMemo(() => times.map((value) => {
    const x = ((value - start) / Math.max(1, end - start)) * 1000;
    return `M${x.toFixed(2)} 4v20`;
  }).join(""), [times, start, end]);
  const inGap = Math.abs(times[index] - cursor) > 1000;

  return <>
    <div className="replay-screen" aria-busy={!currentImage || currentImage.state === "loading"}>
      {currentImage?.src && currentImage.state !== "error" && <img key={imageKey}
        className={ready ? "" : "image-loading"} src={currentImage.src} alt={`电脑截图 ${capturedTime(record)}`}
        onLoad={() => setImage((value) => value?.key === imageKey ? { ...value, state: "ready" } : value)}
        onError={() => setImage((value) => value?.key === imageKey ? { ...value, state: "error" } : value)} />}
      {!ready && <div className="replay-screen-message" role="status">
        {currentImage?.state === "error" ? <><span>截图无法读取，可能已清理或文件不可用</span><button onClick={() => setImageRetry((value) => value + 1)}>重试图片</button></> : "正在加载截图…"}
      </div>}
      <span className="replay-screen-time">{capturedTime(record)}</span>
      <button className="enlarge-button" disabled={!ready} onClick={() => setEnlarged(true)}>放大查看</button>
    </div>
    <div className="replay-metadata">
      <span className="replay-count">{index + 1} / {records.length} 张</span>
    </div>
    <div className="replay-timeline">
      <div className="timeline-readout"><strong>{capturedTime(record)}</strong><span>{inGap ? "该时刻无截图，显示最近记录" : "拖动时间轴查看截图"}</span></div>
      <div className="timeline-track">
        <svg viewBox="0 0 1000 28" preserveAspectRatio="none" aria-hidden="true"><path d={ticks} stroke="currentColor" strokeWidth="2" /></svg>
        <span className="timeline-cursor" style={{ left: `${position(cursor)}%` }} />
        <input type="range" aria-label="回放时间轴" aria-valuetext={`截图时间 ${capturedTime(record)}${inGap ? "，该时刻无截图，显示最近记录" : ""}`}
          min={0} max={Math.max(1, (end - start) / 1000)} step="any" value={(cursor - start) / 1000} disabled={start === end}
          onChange={(event) => seek(start + Number(event.target.value) * 1000)}
          onKeyDown={(event) => {
            let next: number;
            if (event.key === "ArrowLeft" || event.key === "ArrowDown") next = Math.max(0, index - 1);
            else if (event.key === "ArrowRight" || event.key === "ArrowUp") next = Math.min(records.length - 1, index + 1);
            else if (event.key === "Home") next = 0;
            else if (event.key === "End") next = records.length - 1;
            else return;
            event.preventDefault();
            setIndex(next);
            setCursor(times[next]);
          }} />
      </div>
      <div className="timeline-labels"><span>{axisTime(start)}</span>{end - start > 60_000 && <span>{axisTime((start + end) / 2)}</span>}<span>{axisTime(end)}</span></div>
    </div>
    {enlarged && ready && currentImage && <ImageDialog src={currentImage.src} record={record} onClose={() => setEnlarged(false)} />}
  </>;
}

function ImageDialog({ src, record, onClose }: { src: string; record: CaptureRecord; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [original, setOriginal] = useState(false);
  useEffect(() => { dialog.current?.showModal(); }, []);
  return <dialog ref={dialog} className="replay-image-dialog" aria-labelledby="image-dialog-title" onClose={onClose}>
    <div className="dialog-heading">
      <h3 id="image-dialog-title">截图预览 · {capturedTime(record)}</h3>
      <div className="image-dialog-actions"><button onClick={() => setOriginal((value) => !value)}>{original ? "适应窗口" : "原始尺寸"}</button><button onClick={onClose}>关闭</button></div>
    </div>
    <div className={`image-dialog-body ${original ? "original-size" : ""}`}><img src={src} alt={`${capturedTime(record)} 的电脑截图`} /></div>
  </dialog>;
}
