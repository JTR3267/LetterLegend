﻿using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;
using Google.Protobuf;
using Protos.Control;
using Protos.Game;
using Protos.Lobby;
using Unity.VisualScripting;
using UnityEngine;

namespace IO.Net
{
    public class GameTcpClient
    {
        private readonly string _host;
        private readonly int _port;
        private readonly TcpClient _client;
        private readonly Task _thread;
        public GameTcpClient(string host, int port)
        {
            _host = host;
            _port = port;
            _client = new TcpClient();
            State = new StateBroadcast(this);
            Task.Run(async () =>
            {
                var stream = _client.GetStream();
                while (true)
                {
                    var buf = new byte[4];
                    var n = await stream.ReadAsync(buf);
                    if (n != buf.Length)
                        throw new WrongProtocolException();
                    var resLength = BitConverter.ToUInt32(buf);
                    if (resLength == 0)
                        return Array.Empty<byte>();
                    buf = new byte[resLength];
                    n = await stream.ReadAsync(buf);
                    if (n != buf.Length)
                        throw new WrongProtocolException();
                    await State.ExecAsync(buf);
                }
            });
        }
        
        public State State { get; private set; }
        
        public void TransitionTo(State state)
        {
            State = state;
        }

        // public void Handle()
        // {
        //     if (State == null)
        //     {
        //         State = new StateBroadcast();
        //         State.Client = this;
        //     }
        //     State.Handle();
        // }

        public async Task ConnectAsync(string name)
        {
            await _client.ConnectAsync(_host, _port);
            var req = new ConnectRequest()
            {
                Name = name
            };
            var stream = new MemoryStream();
            req.WriteTo(stream);
            await Rpc(Operation.Connect, stream.ToArray());
            
        }

        public async Task<List<LobbyInfo>> GetLobbies()
        {
            var res = ListResponse.Parser.ParseFrom(await Rpc(Operation.ListLobby));
            if (!res.Success)
            {
                throw new Exception("get lobby list fail");
            }
            return res.LobbyInfos.LobbyInfos_.ToList();
        }

        public async Task<byte[]> ReadBroadcast(CancellationToken token = default)
        {
            if (token.IsCancellationRequested)
            {
                Debug.Log("terminate the loop");
                throw new OperationCanceledException(token);  
            }
            var result = await ReadRpcResponse(token);
            return result;
        }
        
        public async Task<Lobby> CreateLobby(uint maxPlayers)
        {
            var req = new CreateRequest()
            {
                MaxPlayers = maxPlayers
            };
            
            var stream = new MemoryStream();
            req.WriteTo(stream);
            var res = CreateResponse.Parser.ParseFrom(await Rpc(Operation.CreateLobby, stream.ToArray()));
            if (!res.Success)
            {
                throw new Exception("create room failed");
            }

            return res.Lobby;
        }
        
        public async Task<Lobby> JoinLobby(uint lobbyId)
        {
            var req = new JoinRequest()
            {
                LobbyId = lobbyId
            };
            
            var stream = new MemoryStream();
            req.WriteTo(stream);
            var res = JoinResponse.Parser.ParseFrom(await Rpc(Operation.JoinLobby, stream.ToArray()));
            if (!res.Success)
            {
                throw new Exception("join room failed");
            }

            return res.Lobby;
        }
        
        public async Task QuitLobby()
        {
            var res = QuitResponse.Parser.ParseFrom(await Rpc(Operation.QuitLobby));
            if (!res.Success)
            {
                throw new Exception("Quit lobby failed");
            }
        }

        public async Task<Protos.Game.Board> Start()
        {
            var res = StartResponse.Parser.ParseFrom(await Rpc(Operation.StartGame));
            if (!res.Success)
            {
                throw new Exception("Someone is not Ready");
            }
            return res.Board;
        }
        
        public Task Reconnect()
        {
            throw new NotImplementedException();
        }

        public async Task Disconnect()
        {
            var res = DisconnectResponse.Parser.ParseFrom(await Rpc(Operation.Disconnect));
            if (!res.Success)
            {
                throw new Exception("disconnect failed");
            }
            _client.Close();
        }
        
        public async Task<byte[]> Rpc(Operation operation, bool readResponse = true)
        {
            return await Rpc(operation, Array.Empty<byte>(), readResponse);
        }

        public async Task<byte[]> Rpc(Operation operation, byte[] data, bool readResponse = true,
            CancellationToken token = default)
        {
            await RpcCall(operation, data);
            var result = readResponse ? await ReadRpcResponse(token) : null;
            return result;
        }

        private async Task RpcCall(Operation operation, byte[] data)
        {
            var stream = _client.GetStream();
            var outputStream = new MemoryStream();
            await outputStream.WriteAsync(new byte[] { (byte)operation, 0, 0, 0 });
            await outputStream.WriteAsync(BitConverter.GetBytes(data.Length));
            await outputStream.WriteAsync(data);
            await stream.WriteAsync(outputStream.ToArray());
        }
        
        private async Task<byte[]> ReadRpcResponse(CancellationToken token = default)
        {
            var stream = _client.GetStream();
            var buf = new byte[4];
            token.ThrowIfCancellationRequested();
            Debug.Log("stuck before readasync");
            var n = await stream.ReadAsync(buf, token);
            Debug.Log("after readasync");
            token.ThrowIfCancellationRequested();
            if (n != buf.Length)
                throw new WrongProtocolException();
            var resLength = BitConverter.ToUInt32(buf);
            if (resLength == 0)
                return Array.Empty<byte>();
            buf = new byte[resLength];
            n = await stream.ReadAsync(buf, token);
            token.ThrowIfCancellationRequested();
            if (n != buf.Length)
                throw new WrongProtocolException();
            return buf;
        }
    }
}