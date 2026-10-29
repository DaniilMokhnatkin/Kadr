"""Rebuild committed vector masters, pixel-fitted ICO frames and the review sheet. Requires Pillow."""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont
from io import BytesIO
import struct

HERE = Path(__file__).resolve().parent
SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
PATH = 'M6 3H2V7 M10 3H14V7 M14 9V13H10 M6 13H2V9'

def render(size, kind):
    # Optical tray masters: outer bounds and the two-pixel stem snap to the physical grid.
    aa = 4 if kind == 'app' else 1
    image = Image.new('RGBA', (size * aa, size * aa))
    draw = ImageDraw.Draw(image)
    if kind == 'app':
        draw.rounded_rectangle((0, 0, size * aa - 1, size * aa - 1), radius=size * aa * .20, fill='#111111')
    ink = '#111111' if kind == 'light' else '#ffffff'
    def box(x0, y0, x1, y1):
        if kind == 'app':
            transform = lambda n: (2.56 + n * .68) * size * aa / 16
        else:
            transform = lambda n: round(n * size / 16)
        left, top, right, bottom = (transform(n) for n in (x0, y0, x1, y1))
        draw.rectangle((left, top, right - 1, bottom - 1), fill=ink)
    for rect in ((1,2,6,4),(1,2,3,7),(10,2,15,4),(13,2,15,7),
                 (13,9,15,14),(10,12,15,14),(1,9,3,14),(1,12,6,14)):
        box(*rect)
    return image.resize((size,size),Image.Resampling.LANCZOS) if aa != 1 else image

def ico(path, kind):
    frames = []
    for size in SIZES:
        data=BytesIO(); render(size,kind).save(data,format='PNG'); frames.append(data.getvalue())
    header = struct.pack('<HHH',0,1,len(frames))
    offset = 6 + len(frames)*16
    entries=[]
    for size,data in zip(SIZES,frames):
        entries.append(struct.pack('<BBBBHHII',size%256,size%256,0,0,1,32,len(data),offset))
        offset += len(data)
    content=header+b''.join(entries)+b''.join(frames)
    if not path.exists() or path.read_bytes()!=content:
        path.write_bytes(content)

def svg(kind):
    ink='#111111' if kind=='light' else '#ffffff'
    body=f'<path d="{PATH}" fill="none" stroke="{ink}" stroke-width="2" stroke-linejoin="miter"/>'
    if kind=='app':
        body='<rect width="16" height="16" rx="3.2" fill="#111111"/>'+f'<g transform="translate(2.56 2.56) scale(.68)">{body}</g>'
    return '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" role="img" aria-label="Kadr — рамка кадра">'+body+'</svg>\n'

for kind,name in (('app','kadr'),('dark','tray-dark'),('light','tray-light')):
    ico(HERE/f'{name}.ico',kind)
    (HERE/f'{name}.svg').write_text(svg(kind),encoding='utf-8')
render(256,'app').save(HERE/'kadr.png')

def font(size):
    return ImageFont.truetype(r'C:\Windows\Fonts\segoeui.ttf',size)
sheet=Image.new('RGB',(840,460),'#fafafa'); draw=ImageDraw.Draw(sheet)
draw.text((32,22),'Kadr',font=font(26),fill='#111111')
draw.text((32,58),'Editorial · рамка кадра',font=font(14),fill='#666666')
sheet.paste(render(160,'app'),(32,108),render(160,'app'))
draw.text((32,288),'Иконка приложения',font=font(14),fill='#111111')
draw.text((32,311),'SVG и ICO · 16–256 px',font=font(12),fill='#666666')
for row,(kind,bg,fg) in enumerate((('dark','#111111','#ffffff'),('light','#f0f0f0','#111111'))):
    y=101+row*136
    draw.rounded_rectangle((240,y,808,y+122),radius=8,fill=bg)
    draw.text((256,y+12),'Тёмная панель задач' if row==0 else 'Светлая панель задач',font=font(13),fill=fg)
    for x,size in zip((270,344,418,492,566,640),(16,20,24,32,40,48)):
        mark=render(size,kind); sheet.paste(mark,(x,y+39+(48-size)//2),mark)
        draw.text((x,y+100),str(size),font=font(11),fill=fg)
draw.text((240,381),'16 px · увеличение ×4',font=font(13),fill='#666666')
for x,kind,bg in ((500,'dark','#111111'),(650,'light','#f0f0f0')):
    square=Image.new('RGBA',(128,128),bg); square.alpha_composite(render(16,kind).resize((128,128),Image.Resampling.NEAREST))
    sheet.paste(square.convert('RGB').resize((64,64),Image.Resampling.NEAREST),(x,375))
sheet.save(HERE/'review.png')
for kind in ('dark','light'):
    for size in SIZES:
        im=render(size,kind)
        assert im.getbbox() is not None and im.getbbox()[0]>0 and im.getbbox()[2]<size
        if kind=='dark': assert any(p[3]==255 and p[:3]==(255,255,255) for p in im.get_flattened_data())
print('Generated 3 ICO families, 9 frames each; SVG masters, PNG and review sheet.')
