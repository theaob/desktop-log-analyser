import { useEffect, useMemo, useRef, useState } from "react";
import uPlot from "uplot";
import { LEVELS, type Level } from "../api";
import { useStore } from "../store";
import { formatCount, formatTs } from "../time";

const HEIGHT = 140;

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#888";
}

/** Log volume over time, stacked by level. Drag to zoom into a time range. */
export default function Histogram() {
  const histogram = useStore((s) => s.histogram);
  const total = useStore((s) => s.total);
  const counting = useStore((s) => s.counting);
  const setRange = useStore((s) => s.setRange);
  const host = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const [hover, setHover] = useState<number | null>(null);

  const prepared = useMemo(() => {
    if (!histogram || histogram.buckets.length === 0) return null;
    const present: Level[] = LEVELS.filter((_, li) => histogram.buckets.some((b) => b[li] > 0));
    const xs = histogram.buckets.map((_, i) => (histogram.start + i * histogram.step) / 1000);
    // Cumulative sums, drawn tallest first so each level's segment stays visible.
    const cum: number[][] = present.map(() => new Array(xs.length).fill(0));
    histogram.buckets.forEach((b, i) => {
      let acc = 0;
      present.forEach((lvl, pi) => {
        acc += b[LEVELS.indexOf(lvl)];
        cum[pi][i] = acc;
      });
    });
    const order = present.map((_, i) => i).reverse();
    return { xs, present, order, cum };
  }, [histogram]);

  useEffect(() => {
    const el = host.current;
    if (!el || !prepared || !histogram) return;
    const { xs, present, order, cum } = prepared;
    const bars = uPlot.paths.bars!({ size: [0.9, 64], align: 1 });
    const opts: uPlot.Options = {
      width: el.clientWidth,
      height: HEIGHT,
      legend: { show: false },
      cursor: { drag: { x: true, y: false, setScale: false }, points: { show: false } },
      scales: { x: { time: true, range: [xs[0], xs[xs.length - 1] + histogram.step / 1000] } },
      axes: [
        { stroke: cssVar("--muted"), grid: { show: false }, ticks: { stroke: cssVar("--border") } },
        { stroke: cssVar("--muted"), grid: { stroke: cssVar("--border"), width: 1 }, size: 50, ticks: { show: false } },
      ],
      series: [
        {},
        ...order.map((pi) => ({
          label: present[pi],
          fill: cssVar(`--lvl-${present[pi]}`),
          stroke: cssVar(`--lvl-${present[pi]}`),
          width: 0,
          paths: bars,
          points: { show: false },
        })),
      ],
      hooks: {
        setSelect: [
          (u) => {
            if (u.select.width < 3) return;
            const from = Math.floor(u.posToVal(u.select.left, "x") * 1000);
            const to = Math.ceil(u.posToVal(u.select.left + u.select.width, "x") * 1000);
            u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
            if (to > from) setRange({ kind: "absolute", from, to });
          },
        ],
        setCursor: [(u) => setHover(u.cursor.idx ?? null)],
      },
    };
    const data: uPlot.AlignedData = [xs, ...order.map((pi) => cum[pi])];
    plot.current?.destroy();
    plot.current = new uPlot(opts, data, el);
    const ro = new ResizeObserver(() => plot.current?.setSize({ width: el.clientWidth, height: HEIGHT }));
    ro.observe(el);
    return () => {
      ro.disconnect();
      plot.current?.destroy();
      plot.current = null;
    };
  }, [prepared, histogram, setRange]);

  if (!histogram) return <div className="histogram empty" />;
  const bucket = hover !== null ? histogram.buckets[hover] : null;
  return (
    <section className="histogram">
      <div className="histogram-legend">
        {bucket ? (
          <>
            <span>{formatTs(histogram.start + hover! * histogram.step, false)}</span>
            {LEVELS.map((l, i) =>
              bucket[i] ? (
                <span key={l} className={`lvl-text-${l}`}>
                  {l} {formatCount(bucket[i])}
                </span>
              ) : null,
            )}
          </>
        ) : (
          <>
            <span>{total !== null && !counting ? `${formatCount(total)} lines` : "Counting…"}</span>
            {prepared?.present.map((l) => (
              <span key={l} className={`lvl-text-${l}`}>
                ■ {l}
              </span>
            ))}
            <span className="muted">Drag to zoom</span>
          </>
        )}
      </div>
      <div ref={host} className="histogram-plot" />
    </section>
  );
}
