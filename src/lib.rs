use once_cell::sync::OnceCell;
use std::{path::Path, sync::RwLock};
use thiserror::Error;

use xcb::{
    x::{
        Atom, ChangeProperty, ClearArea, CreateGc, CreatePixmap, Drawable, Gcontext, GetImage,
        GetProperty, InternAtom, KillClient, Pixmap, PropMode, PutImage, Window, ATOM_NONE,
        ATOM_PIXMAP,
    },
    Connection, Xid, XidNew,
};

#[derive(Error, Debug)]
pub enum ShadeError {
    #[error("Xorg roots iterator did not provided any screens")]
    NoScreenFound,

    #[error("XCB Interal error: {0}")]
    XCBInteral(#[from] xcb::Error),

    #[error("IO Error: {0}")]
    IO(#[from] std::io::Error),

    #[error("Image Error: {0}")]
    Image(#[from] image::error::ImageError),
}

pub type Result<T> = std::result::Result<T, ShadeError>;

pub struct BackgroundHandle {
    pub(crate) context: Gcontext,
    pub(crate) background_pixmap: Pixmap,
    pub(crate) connection: Connection,
    pub(crate) root: Window,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub buffer: RwLock<Box<[u8]>>,
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

struct Molecule {
    atom: Atom,
    pmap: Option<Pixmap>,
}

fn pixmap_from_atom(connection: &Connection, atom_name: &str, window: Window) -> Result<Molecule> {
    let cookie = connection.send_request(&InternAtom {
        name: atom_name.as_bytes(),
        only_if_exists: true,
    });

    let atom = connection
        .wait_for_reply(cookie)
        .map(|reply| reply.atom())?;

    let property_request = connection.send_request(&GetProperty {
        window,
        delete: false,
        property: atom,
        r#type: ATOM_PIXMAP,
        long_offset: 0,
        long_length: 1,
    });

    let property_response = connection.wait_for_reply(property_request)?;

    let pmap = if property_response.r#type() == ATOM_NONE {
        None
    } else {
        Some(unsafe { Pixmap::new(property_response.value()[0]) })
    };

    Ok(Molecule { atom, pmap })
}

fn copy_pixmap(
    source: Pixmap,
    target: Pixmap,
    connection: &Connection,
    gc: Gcontext,
    width: u16,
    height: u16,
) -> Result<()> {
    let get_image_request = GetImage {
        x: 0,
        y: 0,
        width,
        height,
        format: xcb::x::ImageFormat::ZPixmap,
        drawable: Drawable::Pixmap(source),
        plane_mask: !0,
    };

    let get_image_response = connection.send_request(&get_image_request);
    let image_data = connection.wait_for_reply(get_image_response)?;

    let get_image_request = PutImage {
        dst_x: 0,
        dst_y: 0,
        width,
        height,
        left_pad: 0,
        format: xcb::x::ImageFormat::ZPixmap,
        drawable: Drawable::Pixmap(target),
        depth: image_data.depth(),
        data: image_data.data(),
        gc,
    };

    connection
        .send_and_check_request(&get_image_request)
        .map_err(xcb::Error::from)?;

    Ok(())
}
fn inner_load(open_method: OpenMethod) -> Result<BackgroundHandle> {
    let (connection, screen_number) = Connection::connect(None).map_err(xcb::Error::from)?;

    let screen = connection
        .get_setup()
        .roots()
        .nth(screen_number as usize)
        .ok_or(ShadeError::NoScreenFound)?;

    let root = screen.root();
    let width = screen.width_in_pixels();
    let height = screen.height_in_pixels();

    let shade_pmap = {
        let pid = connection.generate_id();
        let request = CreatePixmap {
            depth: screen.root_depth(),
            pid,
            width,
            height,
            drawable: Drawable::Window(root),
        };

        connection
            .send_and_check_request(&request)
            .map_err(xcb::Error::from)?;

        pid
    };

    let gc = {
        let cid = connection.generate_id();
        let request = CreateGc {
            drawable: Drawable::Pixmap(shade_pmap),
            cid,
            value_list: &[],
        };
        connection.send_request(&request);
        cid
    };

    let xrootpmap = pixmap_from_atom(&connection, "_XROOTPMAP_ID", root)?;
    let esetroot_pmap = pixmap_from_atom(&connection, "ESETROOT_PMAP_ID", root)?;
    let pmap = xrootpmap.pmap.or(esetroot_pmap.pmap);

    if pmap.is_some() {
        let pmap_id = pmap.unwrap();

        if let OpenMethod::KeepExisting = open_method {
            copy_pixmap(pmap_id, shade_pmap, &connection, gc, width, height)?;
        }

        let kill_request = KillClient {
            resource: pmap_id.resource_id(),
        };

        connection
            .send_and_check_request(&kill_request)
            .map_err(xcb::Error::from)?;
    }

    if let OpenMethod::LoadFromFile(_method, _path) = open_method {
        todo!("Loading from file is not supported yet");
        // let path = path.as_ref();
        // let data = ImageReader::open(path)?.decode()?;
    }

    connection
        .send_and_check_request(&ChangeProperty {
            mode: PropMode::Replace,
            window: root,
            property: xrootpmap.atom,
            r#type: ATOM_PIXMAP,
            data: &[shade_pmap.resource_id()],
        })
        .map_err(xcb::Error::from)?;

    connection
        .send_and_check_request(&ChangeProperty {
            mode: PropMode::Replace,
            window: root,
            property: esetroot_pmap.atom,
            r#type: ATOM_PIXMAP,
            data: &[shade_pmap.resource_id()],
        })
        .map_err(xcb::Error::from)?;

    connection
        .send_and_check_request(&ClearArea {
            window: root,
            x: 0,
            y: 0,
            width: screen.width_in_pixels(),
            height: screen.height_in_pixels(),
            exposures: true,
        })
        .map_err(xcb::Error::from)?;

    connection.flush().map_err(xcb::Error::from)?;

    let handle = BackgroundHandle {
        connection,
        width,
        height,
        root,
        buffer: RwLock::new(vec![0; height as usize * width as usize].into_boxed_slice()),
        background_pixmap: shade_pmap,
        context: gc,
    };

    Ok(handle)
}

pub fn load(options: OpenMethod) -> Result<&'static BackgroundHandle> {
    static HANDLE: OnceCell<BackgroundHandle> = OnceCell::new();
    HANDLE.get_or_try_init(|| inner_load(options))
}
