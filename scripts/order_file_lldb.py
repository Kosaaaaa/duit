import json
import os
import sys
from pathlib import Path
from typing import Any

import lldb

entered: list[int] = []


def record_entry(
    frame: lldb.SBFrame,
    location: lldb.SBBreakpointLocation,
    extra_args: lldb.SBStructuredData,
    internal_dict: dict[str, Any],
) -> bool:
    entered.append(frame.GetSymbol().GetStartAddress().GetFileAddress())
    location.SetEnabled(False)
    return False


def __lldb_init_module(debugger: lldb.SBDebugger, internal_dict: dict[str, Any]) -> None:
    target = debugger.CreateTarget(os.environ["ORDER_FILE_BINARY"])
    for symbol in target.GetModuleAtIndex(0):
        if symbol.GetType() == lldb.eSymbolTypeCode and symbol.GetStartAddress().IsValid():
            entry_breakpoint = target.BreakpointCreateBySBAddress(symbol.GetStartAddress())
            entry_breakpoint.SetScriptCallbackFunction(f"{__name__}.record_entry")
    launch = target.GetLaunchInfo()
    launch.SetArguments(json.loads(os.environ["ORDER_FILE_ARGUMENTS"]), False)
    debugger.SetAsync(False)
    error = lldb.SBError()
    process = target.Launch(launch, error)
    if not error.Success():
        sys.exit(f"lldb could not launch duit: {error}")
    while process.GetState() == lldb.eStateStopped:
        process.Continue()
    if process.GetState() != lldb.eStateExited or process.GetExitStatus() != 0:
        sys.exit(f"duit did not exit with 0 under lldb: {process}")
    Path(os.environ["ORDER_FILE_OUTPUT"]).write_text("".join(f"{address}\n" for address in entered))
