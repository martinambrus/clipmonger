// Fixed default-profile boundary. Compiled by Windows PowerShell, not an executable.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;

public static class ClipmongerProfileRemoval {
    const uint ReadControl = 0x20000, Synchronize = 0x100000, Delete = 0x10000;
    const uint List = 1, ReadAttributes = 0x80, ShareReadWrite = 3;
    const uint Directory = 1, NonDirectory = 0x40, Sync = 0x20, OpenReparse = 0x200000;
    const uint Reparse = 0x400, DirectoryAttribute = 0x10;
    const int StatusNoMoreFiles = unchecked((int)0x80000006);
    static readonly Guid Roaming = new Guid("3EB685DB-65F9-4CF6-A03A-E3EF65729F3D");
    [StructLayout(LayoutKind.Sequential)] struct UnicodeString { public ushort Length, MaximumLength; public IntPtr Buffer; }
    [StructLayout(LayoutKind.Sequential)] struct ObjectAttributes { public int Length; public IntPtr RootDirectory, ObjectName; public uint Attributes; public IntPtr SecurityDescriptor, SecurityQualityOfService; }
    [StructLayout(LayoutKind.Sequential)] struct IoStatus { public IntPtr Status, Information; }
    [StructLayout(LayoutKind.Sequential)] struct FileInfo { public uint Attributes; public System.Runtime.InteropServices.ComTypes.FILETIME Creation, Access, Write; public uint Volume, SizeHigh, SizeLow, Links, IndexHigh, IndexLow; }
    [DllImport("ntdll.dll")] static extern int NtCreateFile(out SafeFileHandle h, uint access, ref ObjectAttributes oa, out IoStatus io, IntPtr allocation, uint attributes, uint share, uint disposition, uint options, IntPtr ea, uint eaLength);
    [DllImport("ntdll.dll")] static extern int NtQueryDirectoryFile(SafeFileHandle h, IntPtr ev, IntPtr apc, IntPtr context, out IoStatus io, IntPtr buffer, uint length, int infoClass, bool single, IntPtr name, bool restart);
    [DllImport("ntdll.dll")] static extern uint RtlNtStatusToDosError(int status);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern SafeFileHandle CreateFile(string path, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetFileInformationByHandle(SafeFileHandle h, out FileInfo info);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetFileInformationByHandle(SafeFileHandle h, int kind, ref byte data, uint size);
    [DllImport("user32.dll")] static extern IntPtr GetShellWindow();
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool ProcessIdToSessionId(uint pid, out uint session);
    [DllImport("kernel32.dll")] static extern uint GetCurrentProcessId();
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessId(IntPtr process);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("shell32.dll", CharSet=CharSet.Unicode)] static extern int SHGetKnownFolderPath(ref Guid id, uint flags, IntPtr token, out IntPtr path);
    static Exception Error() { return new Win32Exception(Marshal.GetLastWin32Error()); }
    static Exception NtError(int status) { return new Win32Exception((int)RtlNtStatusToDosError(status)); }
    static FileInfo Info(SafeFileHandle h) { FileInfo info; if (!GetFileInformationByHandle(h, out info)) throw Error(); return info; }
    static string Id(FileInfo i) { return i.Volume.ToString("X8") + i.IndexHigh.ToString("X8") + i.IndexLow.ToString("X8"); }
    static void Check(SafeFileHandle h, bool directory) {
        FileInfo i = Info(h);
        if ((i.Attributes & Reparse) != 0 || ((i.Attributes & DirectoryAttribute) != 0) != directory) throw new InvalidOperationException("Reparse point or unexpected object type");
    }
    static SafeFileHandle Relative(SafeFileHandle parent, string name, bool directory, bool deleting = true) {
        if (String.IsNullOrEmpty(name) || name == "." || name == ".." || name.IndexOfAny(new char[]{'\\','/', '\0', ':'}) >= 0) throw new InvalidOperationException("Invalid component");
        IntPtr s = Marshal.StringToHGlobalUni(name), u = IntPtr.Zero;
        try {
            UnicodeString us = new UnicodeString { Length = checked((ushort)(name.Length * 2)), MaximumLength = checked((ushort)(name.Length * 2)), Buffer = s };
            u = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(UnicodeString))); Marshal.StructureToPtr(us, u, false);
            ObjectAttributes oa = new ObjectAttributes { Length = Marshal.SizeOf(typeof(ObjectAttributes)), RootDirectory = parent.DangerousGetHandle(), ObjectName = u, Attributes = 0x40 };
            SafeFileHandle h; IoStatus io;
            int status = NtCreateFile(out h, (deleting ? Delete : 0) | ReadAttributes | Synchronize | (directory ? List : 0), ref oa, out io, IntPtr.Zero, 0, ShareReadWrite, 1, Sync | OpenReparse | (directory ? Directory : NonDirectory), IntPtr.Zero, 0);
            if (status < 0) throw NtError(status);
            try { Check(h, directory); return h; } catch { h.Dispose(); throw; }
        } finally { if (u != IntPtr.Zero) Marshal.FreeHGlobal(u); Marshal.FreeHGlobal(s); }
    }
    sealed class AnchorHandles : IDisposable {
        readonly List<SafeFileHandle> handles = new List<SafeFileHandle>();
        public SafeFileHandle Last { get { return handles[handles.Count - 1]; } }
        public AnchorHandles(string path) {
            string full = System.IO.Path.GetFullPath(path);
            if (full.Length < 3 || full[1] != ':' || full[2] != '\\' || full.StartsWith("\\\\")) throw new InvalidOperationException("Unsupported known folder path");
            SafeFileHandle root = CreateFile(full.Substring(0, 3), List | ReadAttributes | Synchronize, ShareReadWrite, IntPtr.Zero, 3, 0x02200000, IntPtr.Zero);
            if (root.IsInvalid) { root.Dispose(); throw Error(); }
            handles.Add(root);
            try {
                Check(root, true);
                foreach (string part in full.Substring(3).Split(new char[]{'\\'}, StringSplitOptions.RemoveEmptyEntries)) handles.Add(Relative(Last, part, true, false));
            } catch { Dispose(); throw; }
        }
        public void Dispose() { for (int i = handles.Count - 1; i >= 0; --i) handles[i].Dispose(); handles.Clear(); }
    }
    sealed class User : IDisposable {
        public IntPtr Token; public string Sid, Path;
        public void Dispose() { if (Token != IntPtr.Zero) CloseHandle(Token); Token = IntPtr.Zero; }
    }
    static void VerifyShell(IntPtr shell, uint pid, IntPtr process) {
        uint windowPid, shellSession, ourSession;
        if (GetShellWindow() != shell || GetWindowThreadProcessId(shell, out windowPid) == 0 || windowPid != pid ||
            GetProcessId(process) != pid || WaitForSingleObject(process, 0) != 258 ||
            !ProcessIdToSessionId(pid, out shellSession) || !ProcessIdToSessionId(GetCurrentProcessId(), out ourSession) || shellSession != ourSession)
            throw new InvalidOperationException("Interactive shell changed or is unavailable");
    }
    static User CurrentUser() {
        IntPtr shell = GetShellWindow(); uint pid;
        if (shell == IntPtr.Zero || GetWindowThreadProcessId(shell, out pid) == 0) throw new InvalidOperationException("Interactive shell unavailable");
        // Keeping the process handle open pins the PID while its session and token are checked.
        IntPtr process = OpenProcess(0x1000 | 0x100000, false, pid); if (process == IntPtr.Zero) throw Error();
        var user = new User();
        try {
            VerifyShell(shell, pid, process);
            // SHGetKnownFolderPath requires TOKEN_QUERY and TOKEN_IMPERSONATE;
            // TOKEN_DUPLICATE is also needed in some cases.
            if (!OpenProcessToken(process, 0x0002 | 0x0004 | 0x0008, out user.Token)) throw Error();
            using (var shellIdentity = new WindowsIdentity(user.Token)) using (var current = WindowsIdentity.GetCurrent()) {
                user.Sid = shellIdentity.User.Value;
                if (!String.Equals(user.Sid, current.User.Value, StringComparison.Ordinal)) throw new InvalidOperationException("Shell and elevated user differ");
            }
            Guid id = Roaming; IntPtr p = IntPtr.Zero;
            try {
                int hr = SHGetKnownFolderPath(ref id, 0, user.Token, out p);
                if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                user.Path = System.IO.Path.Combine(Marshal.PtrToStringUni(p), "clipmonger");
            } finally { if (p != IntPtr.Zero) Marshal.FreeCoTaskMem(p); }
            VerifyShell(shell, pid, process);
            return user;
        } catch { user.Dispose(); throw; } finally { CloseHandle(process); }
    }
    public sealed class Discovery { public string Path, Sid, AnchorId, TargetId; }
    public static Discovery Discover() {
        using (User user = CurrentUser()) using (WindowsImpersonationContext context = WindowsIdentity.Impersonate(user.Token)) {
            using (AnchorHandles anchors = new AnchorHandles(System.IO.Path.GetDirectoryName(user.Path))) using (SafeFileHandle target = Relative(anchors.Last, "clipmonger", true, false)) {
                return new Discovery { Path = user.Path, Sid = user.Sid, AnchorId = Id(Info(anchors.Last)), TargetId = Id(Info(target)) };
            }
        }
    }
    static List<string> Names(SafeFileHandle directory) {
        var result = new List<string>(); IntPtr buffer = Marshal.AllocHGlobal(65536); bool restart = true;
        try {
            while (true) {
                IoStatus io; int status = NtQueryDirectoryFile(directory, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, out io, buffer, 65536, 12, false, IntPtr.Zero, restart); restart = false;
                if (status == StatusNoMoreFiles) break; if (status < 0) throw NtError(status);
                long size = io.Information.ToInt64(); long offset = 0;
                do {
                    if (offset + 12 > size) throw new InvalidOperationException("Invalid directory response");
                    IntPtr item = new IntPtr(buffer.ToInt64() + offset); uint next = (uint)Marshal.ReadInt32(item); int len = Marshal.ReadInt32(item, 8);
                    if (len < 0 || (len & 1) != 0 || offset + 12L + len > size) throw new InvalidOperationException("Invalid directory name");
                    string name = Marshal.PtrToStringUni(new IntPtr(item.ToInt64() + 12), len / 2); if (name != "." && name != "..") result.Add(name);
                    if (next == 0) break; if (next < 12 || offset + next >= size) throw new InvalidOperationException("Invalid directory offset"); offset += next;
                } while (true);
            }
            return result;
        } finally { Marshal.FreeHGlobal(buffer); }
    }
    static void DisposeEntry(SafeFileHandle parent, string name) {
        SafeFileHandle child = null; bool isDirectory = true;
        try { child = Relative(parent, name, true); }
        catch (Win32Exception e) { if (e.NativeErrorCode != 267) throw; isDirectory = false; child = Relative(parent, name, false); }
        using (child) {
            if (isDirectory) foreach (string entry in Names(child)) DisposeEntry(child, entry);
            byte yes = 1; if (!SetFileInformationByHandle(child, 4, ref yes, 1)) throw Error();
        }
    }
    static void Remove(SafeFileHandle anchor, string expectedAnchor, string expectedTarget) {
        if (!String.Equals(Id(Info(anchor)), expectedAnchor, StringComparison.Ordinal)) throw new InvalidOperationException("Known folder changed");
        using (SafeFileHandle target = Relative(anchor, "clipmonger", true)) {
            if (!String.Equals(Id(Info(target)), expectedTarget, StringComparison.Ordinal)) throw new InvalidOperationException("Profile changed");
            foreach (string entry in Names(target)) {
#if PROFILE_TEST
                if (BeforeOpen != null) { Action<string> hook = BeforeOpen; BeforeOpen = null; hook(entry); }
#endif
                DisposeEntry(target, entry);
            }
            byte yes = 1; if (!SetFileInformationByHandle(target, 4, ref yes, 1)) throw Error();
        }
    }
    static void VerifyIdentity(string expectedSid, string actualSid, string expectedPath, string actualPath) {
        if (String.IsNullOrEmpty(actualSid) || !String.Equals(expectedSid, actualSid, StringComparison.Ordinal) || !String.Equals(expectedPath, actualPath, StringComparison.OrdinalIgnoreCase)) throw new InvalidOperationException("User or profile changed");
    }
    public static void DeleteConfirmed(Discovery expected) {
        if (expected == null) throw new ArgumentNullException("expected");
        using (User user = CurrentUser()) {
            VerifyIdentity(expected.Sid, user.Sid, expected.Path, user.Path);
            using (WindowsImpersonationContext context = WindowsIdentity.Impersonate(user.Token)) using (AnchorHandles anchors = new AnchorHandles(System.IO.Path.GetDirectoryName(user.Path))) Remove(anchors.Last, expected.AnchorId, expected.TargetId);
        }
    }
#if PROFILE_TEST
    public static Action<string> BeforeOpen;
    // Read-only authority probe: never opens profile files or exposes identity/path.
    public static bool TestAuthority() {
        try { using (User user = CurrentUser()) return true; }
        catch { return false; }
    }
    // Compiled only into the fixture executable; production accepts no anchor.
    public static void TestDecision(string anchorPath, bool selected, bool silent, string identity, string expectedIdentity, bool changeTarget) {
        using (AnchorHandles anchors = new AnchorHandles(anchorPath)) {
            string anchorId = Id(Info(anchors.Last)); string targetId;
            using (SafeFileHandle target = Relative(anchors.Last, "clipmonger", true, false)) targetId = Id(Info(target));
            if (changeTarget) targetId = "mismatch";
            if (!selected || silent) return;
            VerifyIdentity(expectedIdentity, identity, anchorPath, anchorPath);
            Remove(anchors.Last, anchorId, targetId);
        }
    }
    public static IDisposable PinFixture(string path) { return new AnchorHandles(path); }
#endif

}
