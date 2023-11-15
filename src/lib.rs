use once_cell::sync::OnceCell;
use std::{path::Path, sync::Mutex};
use thiserror::Error;
use tracing::{info, warn};

use xcb::{
    x::{
        Atom, ChangeProperty, ChangeWindowAttributes, CloseDown::RetainPermanent, CreateGc,
        CreatePixmap, Cw, Drawable, Gcontext, GetProperty, ImageFormat::ZPixmap, InternAtom,
        KillClient, Pixmap, PutImage, SetCloseDownMode, Window, ATOM_ANY, ATOM_NONE, ATOM_PIXMAP, Gc,
    },
    Connection, Xid,
};

// Send a request without reply, check it, and return the error converted into an xcb::Error if
// there is one
macro_rules! void_request {
    ($connection: expr, $request:expr ) => {
        xcb::Connection::send_and_check_request($connection, $request).map_err(xcb::Error::from)
    };
}

// Send a request with reply and wait foro it, check it, and return the error converted into an xcb::Error if
// there is one
macro_rules! cookie_request {
    ($connection: expr, $request:expr) => {{
        let cookie = xcb::Connection::send_request($connection, $request);
        xcb::Connection::wait_for_reply($connection, cookie)
    }};
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Xorg roots iterator did not provided any screens")]
    NoScreenFound,

    #[error("Failed to create root pixmap atoms")]
    FailedRootAtomCreation,

    #[error("XCB Interal error: {0}")]
    XCBInteral(#[from] xcb::Error),

    #[error("IO Error: {0}")]
    IO(#[from] std::io::Error),

    #[error("Image Error: {0}")]
    Image(#[from] image::error::ImageError),
}

pub type Result<T> = std::result::Result<T, Error>;

trait AsByteSlice {
    fn as_byte_slice(&self) -> &[u8];
}

impl<T> AsByteSlice for [T] {
    fn as_byte_slice(&self) -> &[u8] {
        let buffer_ptr = self as *const _ as *const u8;

        unsafe {
            std::slice::from_raw_parts::<u8>(buffer_ptr, std::mem::size_of::<T>() * self.len())
        }
    }
}

#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pixel {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Pixel {
    pub fn new(r: u8, g: u8, b: u8) -> Pixel {
        Pixel { r, g, b }
    }
}

pub enum ScalingMethod {
    Center,
    Fill,
    Max,
    Scale,
    Tile,
}

pub enum OpenMethod<'a> {
    KeepExisting,
    MakeNew,
    LoadFromFile(ScalingMethod, &'a dyn AsRef<Path>),
}

pub struct BackgroundHandle {
    pub(crate) context: Gcontext,
    pub(crate) background_pixmap: Pixmap,
    pub(crate) connection: Connection,
    pub(crate) root: Window,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) depth: u8,
    pub buffer: Mutex<Box<[Pixel]>>,
}

impl BackgroundHandle {
    pub fn flush(&self) -> Result<()> {
        let buffer = self.buffer.lock().unwrap_or_else(|e| e.into_inner());

        void_request!(
            &self.connection,
            &PutImage {
                gc: self.context,
                format: ZPixmap,
                data: buffer.as_byte_slice(),
                width: self.width,
                height: self.height,
                dst_x: 0,
                dst_y: 0,
                depth: self.depth,
                drawable: Drawable::Pixmap(self.background_pixmap),
                left_pad: 0,
            }
        )?;

        Ok(())
    }
}

fn resolve_atom(conn: &Connection, window: Window, atom: Atom) -> xcb::Result<Option<u32>> {
    if atom == ATOM_NONE {
        warn!("Atom {} is NOT SET", atom.resource_id());
        return Ok(None);
    } else {
        let cookie = conn.send_request(&GetProperty {
            r#type: ATOM_ANY,
            delete: false,
            window,
            property: atom,
            long_offset: 0,
            long_length: 1,
        });

        let property = conn.wait_for_reply(cookie)?;

        if property.r#type() != ATOM_PIXMAP {
            warn!("Atom {} is NOT a pixmap", atom.resource_id());
            Ok(None)
        } else {
            let id = match property.format() {
                32 => property.value::<u32>()[0],
                16 => {
                    let bytes = property.value::<u16>().as_byte_slice().try_into().unwrap();
                    u32::from_ne_bytes(bytes)
                }
                8 => {
                    let bytes = property.value::<u8>().try_into().unwrap();
                    u32::from_ne_bytes(bytes)
                }
                _ => unreachable!(),
            };
            Ok(Some(id))
        }
    }
}

fn kill_pmap_atoms(
    connection: &Connection,
    root: Window,
    atom_xroot_pmap: Atom,
    atom_esetroot_pmap: Atom,
) -> xcb::Result<()> {
    // Resolve the ids of the current pixmaps. If anyone is currently drawing to our beloved
    // screen...
    let xrootid = resolve_atom(&connection, root, atom_xroot_pmap)?;
    let esetrootid = resolve_atom(&connection, root, atom_esetroot_pmap)?;

    info!("Foreign pixmaps are X: {xrootid:?} | E: {esetrootid:?}");

    // we MUST kill them
    match (xrootid, esetrootid) {
        (Some(x), Some(e)) => {
            if x == e {
                void_request!(connection, &KillClient { resource: x })?;
            } else {
                void_request!(connection, &KillClient { resource: x })?;
                void_request!(connection, &KillClient { resource: e })?;
            }
        }
        (Some(x), None) => {
            void_request!(connection, &KillClient { resource: x })?;
        }

        (None, Some(e)) => {
            void_request!(connection, &KillClient { resource: e })?;
        }

        (None, None) => {}
    };

    Ok(())
}

fn inner_load(open_method: OpenMethod) -> Result<BackgroundHandle> {
    info!("Connecting to the Xorg Server");
    let (connection, screen_number) = Connection::connect(None).map_err(xcb::Error::from)?;

    let screen = connection
        .get_setup()
        .roots()
        .nth(screen_number as usize)
        .ok_or(Error::NoScreenFound)?;

    let root = screen.root();
    let width = screen.width_in_pixels();
    let height = screen.height_in_pixels();
    let depth = screen.root_depth();

    info!(
        "Root window with id: {}, width: {}, height: {} and depth: {}",
        root.resource_id(),
        width,
        height,
        depth
    );

    let shade_pmap = {
        let pid = connection.generate_id();
        let request = CreatePixmap {
            depth,
            pid,
            width,
            height,
            drawable: Drawable::Window(root),
        };

        void_request!(&connection, &request)?;
        info!("Allocated shade pixmap with id {:?}", pid);

        pid
    };

    let gc = {
        let cid = connection.generate_id();
        let request = CreateGc {
            drawable: Drawable::Pixmap(shade_pmap),
            cid,
            value_list: &[
                Gc::Foreground(screen.white_pixel()),
                Gc::Background(screen.black_pixel())
            ],
        };
        void_request!(&connection, &request)?;
        info!("Allocated shade gc with id {:?}", cid);
        cid
    };

    let mut atom_xroot_pmap = cookie_request!(
        &connection,
        &InternAtom {
            name: b"_XROOTPMAP_ID",
            only_if_exists: true,
        }
    )?
    .atom();

    let mut atom_esetroot_pmap = cookie_request!(
        &connection,
        &InternAtom {
            name: b"ESETROOT_PMAP_ID",
            only_if_exists: true,
        }
    )?
    .atom();

    kill_pmap_atoms(&connection, root, atom_xroot_pmap, atom_esetroot_pmap)?;

    // Create these if they did not exist before (e.g. the previous InternAtom request returned ATOM_NONE)
    atom_xroot_pmap = cookie_request!(
        &connection,
        &InternAtom {
            name: b"_XROOTPMAP_ID",
            only_if_exists: false,
        }
    )?
    .atom();

    atom_esetroot_pmap = cookie_request!(
        &connection,
        &InternAtom {
            name: b"ESETROOT_PMAP_ID",
            only_if_exists: false,
        }
    )?
    .atom();

    if atom_xroot_pmap == ATOM_NONE || atom_esetroot_pmap == ATOM_NONE {
        return Err(Error::FailedRootAtomCreation);
    }

    void_request!(
        &connection,
        &ChangeProperty {
            property: atom_xroot_pmap,
            mode: xcb::x::PropMode::Replace,
            r#type: ATOM_PIXMAP,
            window: root,
            data: &[shade_pmap.resource_id()],
        }
    )?;

    void_request!(
        &connection,
        &ChangeProperty {
            property: atom_esetroot_pmap,
            mode: xcb::x::PropMode::Replace,
            r#type: ATOM_PIXMAP,
            window: root,
            data: &[shade_pmap.resource_id()],
        }
    )?;

    // TODO This might not work on multi monitor setups
    // TODO This also requires the monitor to be cleared

    void_request!(
        &connection,
        &ChangeWindowAttributes {
            window: root,
            value_list: &[Cw::BackPixmap(shade_pmap)],
        }
    )?;

    void_request!(
        &connection,
        &SetCloseDownMode {
            mode: RetainPermanent
        }
    )?;

    connection.flush().map_err(xcb::Error::from)?;

    let handle = BackgroundHandle {
        connection,
        width,
        height,
        depth,
        root,
        buffer: Mutex::new(
            vec![Pixel::default(); height as usize * width as usize].into_boxed_slice(),
        ),
        background_pixmap: shade_pmap,
        context: gc,
    };

    info!("Created handle");

    Ok(handle)
}

pub fn load(options: OpenMethod) -> Result<&'static BackgroundHandle> {
    static HANDLE: OnceCell<BackgroundHandle> = OnceCell::new();
    HANDLE.get_or_try_init(|| inner_load(options))
}
