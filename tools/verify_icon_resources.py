"""Read and load ICO resources from the compiled EXE without starting the application."""
import ctypes as c
from ctypes import wintypes as w
from io import BytesIO
from pathlib import Path
import struct
from PIL import Image

root = Path(__file__).resolve().parents[1]
import sys
exe = Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'dist/Kadr.exe'
kernel = c.WinDLL('kernel32', use_last_error=True)
user = c.WinDLL('user32', use_last_error=True)
kernel.LoadLibraryExW.argtypes=[w.LPCWSTR,w.HANDLE,w.DWORD];kernel.LoadLibraryExW.restype=w.HMODULE
kernel.FindResourceW.argtypes=[w.HMODULE,c.c_void_p,c.c_void_p];kernel.FindResourceW.restype=c.c_void_p
kernel.LoadResource.argtypes=[w.HMODULE,c.c_void_p];kernel.LoadResource.restype=c.c_void_p
kernel.SizeofResource.argtypes=[w.HMODULE,c.c_void_p];kernel.SizeofResource.restype=w.DWORD
kernel.LockResource.argtypes=[c.c_void_p];kernel.LockResource.restype=c.c_void_p
kernel.FreeLibrary.argtypes=[w.HMODULE];kernel.FreeLibrary.restype=w.BOOL
user.LoadImageW.argtypes=[w.HINSTANCE,c.c_void_p,w.UINT,c.c_int,c.c_int,w.UINT];user.LoadImageW.restype=w.HANDLE
user.DestroyIcon.argtypes=[w.HANDLE];user.DestroyIcon.restype=w.BOOL
module=kernel.LoadLibraryExW(str(exe),None,0x2|0x20)
assert module,c.get_last_error()
def resource(kind,ident):
    handle=kernel.FindResourceW(module,c.c_void_p(ident),c.c_void_p(kind))
    assert handle,(kind,ident,c.get_last_error())
    data=kernel.LoadResource(module,handle)
    return c.string_at(kernel.LockResource(data),kernel.SizeofResource(module,handle))
try:
    for ident,name in ((1,'kadr'),(101,'tray-dark'),(102,'tray-light')):
        group=resource(14,ident)
        _,kind,count=struct.unpack_from('<HHH',group)
        assert kind==1 and count==9
        sizes=[]
        for index in range(count):
            width,height,_,_,_,_,byte_count,icon_id=struct.unpack_from('<BBBBHHIH',group,6+14*index)
            width=width or 256;height=height or 256;sizes.append(width)
            raw=resource(3,icon_id)
            assert len(raw)==byte_count
            im=Image.open(BytesIO(raw)).convert('RGBA')
            assert im.size==(width,height)
            source=Image.open(root/f'assets/icons/{name}.ico').ico.getimage((width,height)).convert('RGBA')
            assert im.tobytes()==source.tobytes()
            loaded=user.LoadImageW(module,c.c_void_p(ident),1,width,height,0)
            assert loaded,(name,width,c.get_last_error())
            assert user.DestroyIcon(loaded)
        assert sizes==[16,20,24,32,40,48,64,128,256]
        print(f'{name}: all 9 compiled frames match the asset and load successfully in Windows')
    manifest=resource(24,1)
    assert b'uiAccess="false"' in manifest or b"uiAccess='false'" in manifest
    print('Existing non-uiAccess manifest preserved.')
finally:
    kernel.FreeLibrary(module)
