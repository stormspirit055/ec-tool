import { useEffect, type RefObject } from "react";

/** 主目控制播放、暂停、跳转和倍速；另一目在缓冲后回到同一播放位置。 */
export function useStereoSync(masterRef: RefObject<HTMLVideoElement | null>, peerRef: RefObject<HTMLVideoElement | null>, source: string) {
  useEffect(() => {
    const master = masterRef.current, peer = peerRef.current;
    if (!master || !peer) return;
    let stopped = false, handle = 0, buffering = false;
    const align = () => {
      peer.playbackRate = master.playbackRate;
      if (peer.readyState > 0 && (!peer.seeking || master.seeking) && Math.abs(peer.currentTime - master.currentTime) > 0.04) {
        peer.currentTime = Math.min(master.currentTime, Number.isFinite(peer.duration) ? peer.duration : master.currentTime);
      }
    };
    const sync = () => {
      align();
      if (master.paused || master.ended || buffering) peer.pause();
      else if (peer.paused) peer.play().catch(() => {});
    };
    const waiting = () => { buffering = true; peer.pause(); };
    const playing = () => { buffering = false; sync(); };
    const tick = () => { if (stopped) return; align(); handle = requestAnimationFrame(tick); };
    for (const name of ["play", "pause", "seeking", "seeked", "ratechange", "ended"]) master.addEventListener(name, sync);
    master.addEventListener("waiting", waiting);
    master.addEventListener("playing", playing);
    peer.addEventListener("loadedmetadata", sync);
    peer.addEventListener("canplay", sync);
    handle = requestAnimationFrame(tick);
    sync();
    return () => {
      stopped = true;
      cancelAnimationFrame(handle);
      peer.pause();
      for (const name of ["play", "pause", "seeking", "seeked", "ratechange", "ended"]) master.removeEventListener(name, sync);
      master.removeEventListener("waiting", waiting);
      master.removeEventListener("playing", playing);
      peer.removeEventListener("loadedmetadata", sync);
      peer.removeEventListener("canplay", sync);
    };
  }, [masterRef, peerRef, source]);
}
