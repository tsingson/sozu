#![allow(unused)]
use sozu_command::buffer::fixed::Buffer;
use super::super::buffer::HttpBuffer;

use nom::{HexDisplay,IResult,Offset,Err};

use url::Url;

use std::str;
use std::convert::From;

use super::{BufferMove, LengthInformation, RRequestLine, Connection, Chunk, Host, HeaderValue, TransferEncodingValue,
  Method, Version, Continue, message_header, crlf,};
use super::header::{self, RequestLine, Header, HeaderName, Slice, Meta, request_line};
use protocol::http::AddedRequestHeader;


#[derive(Debug,Clone,PartialEq)]
pub enum RequestState {
  Initial,
  Error {
      request: Option<RequestLine>, headers: Vec<Header>, data: Option<Slice>, index: usize,
      host: Option<Host>, connection: Option<Connection>, length: Option<LengthInformation>,
      chunk: Option<Chunk> },
  // index is how far we have parsed in the buffer
  Parsing { request: RequestLine, headers: Vec<Header>, index: usize },
  ParsingDone { request: RequestLine, headers: Vec<Header>, data: Slice, index: usize  },
  Copying { request: Option<RequestLine>, headers: Vec<Header>, data: Slice, index: usize, connection: Connection, host: Host, length: LengthInformation },
  Request(RRequestLine, Connection, Host),
  RequestWithBody(RRequestLine, Connection, Host, usize),
  RequestWithBodyChunks(RRequestLine, Connection, Host, Chunk),
}

impl RequestState {
  pub fn into_error(self) -> RequestState {
    match self {
      RequestState::Initial => RequestState::Error {
          request: None, headers: Vec::new(), data: None, index: 0,
          host: None, connection: None, length: None, chunk: None
      },
      RequestState::Parsing { request, headers, index } => RequestState::Error {
          request: Some(request), headers, data: None, index,
          host: None, connection: None, length: None, chunk: None
      },
      RequestState::ParsingDone { request, headers, data, index } => RequestState::Error {
          request: Some(request), headers, data: Some(data), index,
          host: None, connection: None, length: None, chunk: None,
      },
      RequestState::Request(rl, conn, host)  => RequestState::Error {
          request: None, headers: Vec::new(), data: None, index: 0,
          host: Some(host), connection: Some(conn), length: None, chunk: None
      },
      RequestState::RequestWithBody(rl, conn, host, len) => RequestState::Error {
          request: None, headers: Vec::new(), data: None,  index: 0,
          host: Some(host), connection: Some(conn), length: Some(LengthInformation::Length(len)), chunk: None
      },
      RequestState::RequestWithBodyChunks(rl, conn, host, chunk) => RequestState::Error {
          request: None, headers: Vec::new(), data: None,  index: 0,
          host: Some(host), connection: Some(conn), length: Some(LengthInformation::Chunked), chunk: Some(chunk)
      },
      err => err,
    }
  }

  pub fn is_front_error(&self) -> bool {
    if let RequestState::Error { .. } = self {
      true
    } else {
      false
    }
  }

  pub fn get_sticky_session(&self) -> Option<&str> {
    //self.get_keep_alive().and_then(|con| con.sticky_session.as_ref()).map(|s| s.as_str())
    unimplemented!()
  }

  pub fn has_host(&self) -> bool {
    match *self {
      //RequestState::HasHost(_, _, _)            |
      //RequestState::HasHostAndLength(_, _, _, _)|
      RequestState::Request(_, _, _)            |
      RequestState::RequestWithBody(_, _, _, _) |
      RequestState::RequestWithBodyChunks(_, _, _, _) => true,
      _                                               => false
    }
  }

  pub fn is_proxying(&self) -> bool {
    match *self {
      RequestState::Request(_, _, _)            |
      RequestState::RequestWithBody(_, _, _, _) |
      RequestState::RequestWithBodyChunks(_, _, _, _)  => true,
      _                                                => false
    }
  }

  pub fn is_head(&self) -> bool {
    match *self {
      RequestState::Request(ref rl, _, _)            |
      RequestState::RequestWithBody(ref rl, _, _, _) |
      RequestState::RequestWithBodyChunks(ref rl, _, _, _) => {
        rl.method == Method::Head
      },
      _                                                => false
    }
  }

  pub fn get_host(&self) -> Option<&str> {
    match *self {
      //RequestState::HasHost(_, _, ref host)             |
      //RequestState::HasHostAndLength(_, _, ref host, _) |
      RequestState::Request(_, _, ref host)             |
      RequestState::RequestWithBody(_, _, ref host, _)  |
      RequestState::RequestWithBodyChunks(_, _, ref host, _) => Some(host.as_str()),
      RequestState::Error { ref host, .. }                    => host.as_ref().map(|s| s.as_str()),
      _                                                      => None
    }
  }

  pub fn get_uri(&self) -> Option<&str> {
    match *self {
      //RequestState::HasRequestLine(ref rl, _)         |
      //RequestState::HasHost(ref rl, _, _)             |
      //RequestState::HasHostAndLength(ref rl, _, _, _) |
      RequestState::Request(ref rl , _, _)            |
      RequestState::RequestWithBody(ref rl, _, _, _)  |
      RequestState::RequestWithBodyChunks(ref rl, _, _, _) => Some(rl.uri.as_str()),
      //FIXME
      //RequestState::Error{ rl, .. }              => rl.as_ref().map(|r| r.uri.as_str()),
      _                                                    => None
    }
  }

  pub fn get_request_line(&self) -> Option<&RRequestLine> {
    match *self {
      //RequestState::HasRequestLine(ref rl, _)         |
      //RequestState::HasHost(ref rl, _, _)             |
      //RequestState::HasHostAndLength(ref rl, _, _, _) |
      RequestState::Request(ref rl, _, _)             |
      RequestState::RequestWithBody(ref rl, _, _, _)  |
      RequestState::RequestWithBodyChunks(ref rl, _, _, _) => Some(rl),
      //FIXME
      //RequestState::Error{ rl, .. }           => rl.as_ref(),
      _                                                    => None
    }
  }

  pub fn get_keep_alive(&self) -> Option<&Connection> {
    match *self {
      //RequestState::HasRequestLine(_, ref conn)         |
      //RequestState::HasHost(_, ref conn, _)             |
      //RequestState::HasLength(_, ref conn, _)           |
      //RequestState::HasHostAndLength(_, ref conn, _, _) |
      RequestState::Request(_, ref conn, _)             |
      RequestState::RequestWithBody(_, ref conn, _, _)  |
      RequestState::RequestWithBodyChunks(_, ref conn, _, _) => Some(conn),
      RequestState::Error{ ref connection, .. }       => connection.as_ref(),
      _                                                      => None
    }
  }

  pub fn get_mut_connection(&mut self) -> Option<&mut Connection> {
    match *self {
      /*RequestState::HasRequestLine(_, ref mut conn)         |
      RequestState::HasHost(_, ref mut conn, _)             |
      RequestState::HasLength(_, ref mut conn, _)           |
      RequestState::HasHostAndLength(_, ref mut conn, _, _) |*/
      RequestState::Request(_, ref mut conn, _)             |
      RequestState::RequestWithBody(_, ref mut conn, _, _)  |
      RequestState::RequestWithBodyChunks(_, ref mut conn, _, _) => Some(conn),
      _                                                      => None
    }

  }

  pub fn should_copy(&self, position: usize) -> Option<usize> {
    match *self {
      RequestState::RequestWithBody(_, _, _, l) => Some(position + l),
      RequestState::Request(_, _, _)            => Some(position),
      _                                         => None
    }
  }

  pub fn should_keep_alive(&self) -> bool {
    //FIXME: should not clone here
    let rl =  self.get_request_line();
    let version = rl.as_ref().map(|rl| rl.version);
    let conn = self.get_keep_alive();
    match (version, conn.map(|c| c.keep_alive)) {
      (_, Some(Some(true)))   => true,
      (_, Some(Some(false)))  => false,
      (Some(Version::V10), _) => false,
      (Some(Version::V11), _) => true,
      (_, _)                  => false,
    }
  }

  pub fn as_ioslice(&self, buffer: &[u8]) -> Vec<std::io::IoSlice> {
      unimplemented!()
  }

  pub fn next_slice(&self, buffer: &[u8]) -> &[u8] {
      unimplemented!()
  }

  pub fn output_size(&self, buffer: &[u8]) -> usize {
      unimplemented!()
  }

  pub fn needs_input(&self, buffer: usize) -> bool {
      unimplemented!()
  }
  // argument: how much was written
  // return: how much the buffer should be advanced
  //
  // if we're sending the headers, we do not want to advance
  // the buffer until all have been sent
  // also, if we are deleting a chunk of data, we might return a higher value
  pub fn consume(&mut self, consumed: usize) -> usize {
      unimplemented!()
  }

  pub fn can_restart_parsing(&self, available_data: usize) -> bool {
      unimplemented!()
  }
}

pub fn default_request_result<O>(state: RequestState, res: IResult<&[u8], O>) -> RequestState {
  match res {
    Err(Err::Error(_)) | Err(Err::Failure(_)) => state.into_error(),
    Err(Err::Incomplete(_)) => state,
    _                      => unreachable!()
  }
}

pub fn parse_request_until_stop(mut state: RequestState, mut header_end: Option<usize>,
  buffer: &mut HttpBuffer, added_req_header: Option<&AddedRequestHeader>, sticky_name: &str)
  -> (RequestState, Option<usize>) {
  
  let buf = buffer.unparsed_data();
  println!("will parse: {:?}", buf.to_hex(32));

  loop {
    println!("state: {:?}", state);
    match state {
      RequestState::Initial => match header::request_line(buf) {
        Ok((i, (method, uri, version)))    => {
          let rline = RequestLine::new(buf, method, uri, version);
          println!("rline: {:?}", rline);
          state = RequestState::Parsing { request: rline, headers: Vec::new(), index: buf.offset(i) };
        },
        Err(Err::Incomplete(_)) => break,
        res => {
          println!("err: {:?}", res);
          state = default_request_result(state, res);
          break;
        }
      },
      RequestState::Parsing { request, mut headers, index } => match message_header(&buf[index..]) {
        Ok((i, header)) => {
          println!("header: {:?}", header);
          headers.push(Header::new(buf, header.name, header.value));
          state = RequestState::Parsing { request, headers, index: buf.offset(i) };
        },
        Err(_) => {
          match crlf(&buf[index..]) {
            Ok((i, o)) => {
              println!("parsing done");
              state = RequestState::ParsingDone{ request, headers, index: buf.offset(i), data: Slice::new(buf, o, Meta::HeadersEnd) };
              break;
            },
            res => {
              state = default_request_result(RequestState::Parsing { request, headers, index }, res);
              break;
            },
          }
        }
        res => {
          state = default_request_result(RequestState::Parsing { request, headers, index }, res);
          break;
        }
      },
      s => panic!("parse_request_until_stop should not be called with this state: {:?}", s),
    }
  }

  let header_end = if let RequestState::ParsingDone { index, .. } = state {
    Some(index)
  } else {
    None
  };

  state = match state {
    RequestState::ParsingDone{ request, headers, index, data } => {
      finish_request(request, headers, index, data, buffer,
        added_req_header, sticky_name)
    },
    s => s,
  };

  (state, header_end)
}

fn transform_request(request: RequestLine, headers: &[Header], mut header_end: Option<usize>,
  buffer: &mut HttpBuffer, added_req_header: Option<&AddedRequestHeader>, sticky_name: &str)
  -> (RequestLine, Vec<Header>) {
    let h: Vec<Header> = headers.iter()
    //.filter_map(|h| )
    .cloned()
    .collect();
    (request, h)
}

fn finish_request(request: RequestLine, mut headers: Vec<Header>, index: usize, data: Slice,
  buffer: &mut HttpBuffer, added_req_header: Option<&AddedRequestHeader>, sticky_name: &str)
  -> RequestState {
    let mut connection = Connection::new();
    let mut length: Option<LengthInformation> = None;
    let mut host: Option<String> = None;
    let request_line = request.to_rrequest_line(buffer.unparsed_data());
    /*
    Error {
  request: Option<RequestLine>, headers: Vec<Header>, data: Option<Slice>, index: usize,
  host: Option<Host>, connection: Option<Connection>, length: Option<LengthInformation>,
  chunk: Option<Chunk> },
    RequestState::Request(RRequestLine, Connection, Host),
    RequestWithBody(RRequestLine, Connection, Host, usize),
    RequestWithBodyChunks(RRequestLine, Connection, Host, Chunk),
    */

    for header in headers.drain(..) {
      match header.name.meta {
        Meta::HeaderName(HeaderName::Host) => match header.value.data(buffer.unparsed_data()) {
          None =>  unimplemented!(),
          Some(s) => {
            host = std::str::from_utf8(s).ok().map(String::from);
          },
        },
        Meta::HeaderName(HeaderName::ContentLength) => match header.value.data(buffer.unparsed_data()) {
          None =>  unimplemented!(),
          Some(s) => match str::from_utf8(s).ok().and_then(|s| s.parse::<usize>().ok()) {
            None =>  unimplemented!(),
            Some(sz) => if length.is_none() {
              length = Some(LengthInformation::Length(sz));
              // we should allow multiple Content-Length headers if they have the same value
            } else {
              unimplemented!()
            }
          }
        },
        Meta::HeaderName(HeaderName::TransferEncoding) => match header.value.data(buffer.unparsed_data()) {
          None =>  unimplemented!(),
          Some(s) => {
            for value in super::comma_separated_values(s) {
              // Transfer-Encoding gets the priority over Content-Length
              if super::compare_no_case(value, b"chunked") {
                length = Some(LengthInformation::Chunked);
              }
            }
          }
        },
        _ => {},
      };
    }

    if request_line.is_none() {
      unimplemented!();
    }
    let request_line = request_line.unwrap();
    if host.is_none() {
      unimplemented!();
    }

    let state = match length {
      None => RequestState::Request(request_line, connection, host.unwrap()),
      Some(LengthInformation::Length(sz)) => RequestState::RequestWithBody(request_line, connection, host.unwrap(), sz),
      Some(LengthInformation::Chunked) => RequestState::RequestWithBodyChunks(request_line, connection, host.unwrap(), Chunk::Initial),
    };

    state

}