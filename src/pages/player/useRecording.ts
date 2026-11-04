import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { backgroundWrite } from "../../lib/backgroundWrite";
import { usePlayer } from "../../stores/player";

/** Recording live TV (T-035): the file being written, since when, and a toggle. */
export function useRecording(say: (text: string) => void) {
  const recordingFile = usePlayer((s) => s.props.recording);
  const lastRecording = useRef("");
  const [recordingSince, setRecordingSince] = useState(0);
  useEffect(() => {
    if (recordingFile && !lastRecording.current) setRecordingSince(Date.now());
    // ended by the user, a channel change or a failed stream: say where it went
    if (!recordingFile && lastRecording.current) say(`Recording saved: ${lastRecording.current.split("/").pop()}`);
    lastRecording.current = recordingFile;
  }, [recordingFile, say]);
  const toggleRecord = useCallback(() => {
    const on = !usePlayer.getState().props.recording;
    void backgroundWrite(api.record(on), (e) => say(`Can't record: ${e}`));
  }, [say]);
  return { recordingFile, recordingSince, toggleRecord };
}
